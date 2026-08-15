use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use futures_util::StreamExt;
use prost::Message;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

use crate::error::CursorSdkError;

const CONNECT_PROTO_UNARY: &str = "application/proto";
const CONNECT_PROTO_STREAM: &str = "application/connect+proto";
const CONNECT_PROTOCOL_VERSION: &str = "1";

/// Catalog RPCs (ListModels, CreateAgent, …).
const UNARY_TIMEOUT: Duration = Duration::from_secs(60);
/// Send / WaitLiveRun can outlive a long Cursor exploration.
const STREAM_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Unary Connect POST with a raw protobuf body (`application/proto`).
pub async fn unary<Req: Message, Resp: Message + Default>(
    http: &reqwest::Client,
    base_url: &str,
    service: &str,
    method: &str,
    bearer: &str,
    request: &Req,
) -> Result<Resp, CursorSdkError> {
    unary_with_timeout(
        http,
        base_url,
        service,
        method,
        bearer,
        request,
        UNARY_TIMEOUT,
    )
    .await
}

/// Unary Connect POST with an explicit request timeout (WaitLiveRun).
pub async fn unary_with_timeout<Req: Message, Resp: Message + Default>(
    http: &reqwest::Client,
    base_url: &str,
    service: &str,
    method: &str,
    bearer: &str,
    request: &Req,
    timeout: Duration,
) -> Result<Resp, CursorSdkError> {
    let url = format!("{base_url}/{service}/{method}");
    let body = request.encode_to_vec();
    let response = http
        .post(&url)
        .header(AUTHORIZATION, format!("Bearer {bearer}"))
        .header(CONTENT_TYPE, CONNECT_PROTO_UNARY)
        .header("Connect-Protocol-Version", CONNECT_PROTOCOL_VERSION)
        .timeout(timeout)
        .body(body)
        .send()
        .await?;

    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        return Err(connect_error_from_body(service, method, &bytes));
    }
    Ok(Resp::decode(bytes)?)
}

/// Outcome of a Connect server stream: messages seen so far, plus a
/// non-fatal transport error if the stream dropped before the end flag.
pub struct StreamOutcome<Resp> {
    pub messages: Vec<Resp>,
    pub error: Option<CursorSdkError>,
}

/// Server-stream Connect POST (`application/connect+proto`).
///
/// Reads the body incrementally so a long Cursor run is not buffered until
/// EOF (and so a timeout can still return frames already seen, including
/// `run_id` for WaitLiveRun).
pub async fn server_stream<Req: Message, Resp: Message + Default>(
    http: &reqwest::Client,
    base_url: &str,
    service: &str,
    method: &str,
    bearer: &str,
    request: &Req,
) -> Result<Vec<Resp>, CursorSdkError> {
    let outcome = server_stream_outcome(http, base_url, service, method, bearer, request).await?;
    match outcome.error {
        Some(e) if outcome.messages.is_empty() => Err(e),
        _ => Ok(outcome.messages),
    }
}

pub async fn server_stream_outcome<Req: Message, Resp: Message + Default>(
    http: &reqwest::Client,
    base_url: &str,
    service: &str,
    method: &str,
    bearer: &str,
    request: &Req,
) -> Result<StreamOutcome<Resp>, CursorSdkError> {
    server_stream_outcome_on(http, base_url, service, method, bearer, request, |_| {}).await
}

/// Like [`server_stream_outcome`], calling `on_msg` as each response frame arrives.
pub async fn server_stream_outcome_on<Req, Resp, F>(
    http: &reqwest::Client,
    base_url: &str,
    service: &str,
    method: &str,
    bearer: &str,
    request: &Req,
    mut on_msg: F,
) -> Result<StreamOutcome<Resp>, CursorSdkError>
where
    Req: Message,
    Resp: Message + Default,
    F: FnMut(&Resp),
{
    let url = format!("{base_url}/{service}/{method}");
    let envelope = encode_connect_frame(0, &request.encode_to_vec());
    let response = http
        .post(&url)
        .header(AUTHORIZATION, format!("Bearer {bearer}"))
        .header(CONTENT_TYPE, CONNECT_PROTO_STREAM)
        .header("Connect-Protocol-Version", CONNECT_PROTOCOL_VERSION)
        .timeout(STREAM_TIMEOUT)
        .body(envelope)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let bytes = response.bytes().await?;
        return Err(connect_error_from_body(service, method, &bytes));
    }

    let mut decoder = ConnectFrameDecoder::new();
    let mut messages = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                return Ok(StreamOutcome {
                    messages,
                    error: Some(e.into()),
                });
            }
        };
        decoder.push(&chunk);
        let before = messages.len();
        match drain_frames::<Resp>(service, method, &mut decoder, &mut messages) {
            Ok(ended) => {
                for msg in &messages[before..] {
                    on_msg(msg);
                }
                if ended {
                    return Ok(StreamOutcome {
                        messages,
                        error: None,
                    });
                }
            }
            Err(e) => {
                for msg in &messages[before..] {
                    on_msg(msg);
                }
                return Ok(StreamOutcome {
                    messages,
                    error: Some(e),
                });
            }
        }
    }
    Ok(StreamOutcome {
        messages,
        error: None,
    })
}

/// Incremental Connect frame splitter (flags + 4-byte BE length).
pub struct ConnectFrameDecoder {
    buf: BytesMut,
}

impl Default for ConnectFrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectFrameDecoder {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    pub fn take_frames(&mut self) -> Vec<(u8, Bytes)> {
        let mut frames = Vec::new();
        loop {
            if self.buf.len() < 5 {
                break;
            }
            let len = u32::from_be_bytes(self.buf[1..5].try_into().unwrap()) as usize;
            if self.buf.len() < 5 + len {
                break;
            }
            let flags = self.buf[0];
            let _ = self.buf.split_to(5);
            let payload = self.buf.split_to(len).freeze();
            frames.push((flags, payload));
        }
        frames
    }
}

/// Drain complete frames. `Ok(true)` means the Connect end-stream flag arrived.
fn drain_frames<Resp: Message + Default>(
    service: &str,
    method: &str,
    decoder: &mut ConnectFrameDecoder,
    messages: &mut Vec<Resp>,
) -> Result<bool, CursorSdkError> {
    for (flags, payload) in decoder.take_frames() {
        if flags & 0x02 != 0 {
            if !payload.is_empty() {
                let end: EndStreamResponse = serde_json::from_slice(&payload).unwrap_or_default();
                if let Some(err) = end.error {
                    return Err(connect_error(service, method, &err));
                }
            }
            return Ok(true);
        }
        if payload.is_empty() {
            continue;
        }
        match Resp::decode(payload) {
            Ok(msg) => messages.push(msg),
            Err(_) => {
                // Keepalives / unknown envelope versions must not fail the run.
            }
        }
    }
    Ok(false)
}

pub fn encode_connect_frame(flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(flags);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn decode_connect_stream<Resp: Message + Default>(
    service: &str,
    method: &str,
    body: &[u8],
) -> Result<Vec<Resp>, CursorSdkError> {
    let mut buf = Bytes::copy_from_slice(body);
    let mut messages = Vec::new();
    while buf.remaining() >= 5 {
        let flags = buf.get_u8();
        let len = buf.get_u32() as usize;
        if buf.remaining() < len {
            return Err(CursorSdkError::message(format!(
                "{service}/{method}: truncated connect frame"
            )));
        }
        let payload = buf.copy_to_bytes(len);
        if flags & 0x02 != 0 {
            if !payload.is_empty() {
                let end: EndStreamResponse = serde_json::from_slice(&payload).unwrap_or_default();
                if let Some(err) = end.error {
                    return Err(connect_error(service, method, &err));
                }
            }
            break;
        }
        if payload.is_empty() {
            continue;
        }
        if let Ok(msg) = Resp::decode(payload) {
            messages.push(msg);
        }
    }
    Ok(messages)
}

#[derive(Debug, Default, Deserialize)]
struct EndStreamResponse {
    #[serde(default)]
    error: Option<ConnectErrorBody>,
}

#[derive(Debug, Default, Deserialize)]
struct ConnectErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    details: Vec<serde_json::Value>,
}

fn connect_error_from_body(service: &str, method: &str, body: &[u8]) -> CursorSdkError {
    if let Ok(err) = serde_json::from_slice::<ConnectErrorBody>(body) {
        return connect_error(service, method, &err);
    }
    let text = String::from_utf8_lossy(body);
    CursorSdkError::Rpc {
        method: format!("{service}/{method}"),
        code: "unknown".into(),
        message: text.chars().take(500).collect(),
        request_id: None,
        sdk_error_code: None,
    }
}

fn connect_error(service: &str, method: &str, err: &ConnectErrorBody) -> CursorSdkError {
    let (request_id, sdk_error_code) = extract_sdk_error_details(&err.details);
    let mut message = err.message.clone();
    if let Some(code) = sdk_error_code.as_deref() {
        message = format!("{message} [sdk_error_code={code}]");
    }
    if let Some(rid) = request_id.as_deref() {
        message = format!("{message} [request_id={rid}]");
    }
    CursorSdkError::Rpc {
        method: format!("{service}/{method}"),
        code: if err.code.is_empty() {
            "unknown".into()
        } else {
            err.code.clone()
        },
        message,
        request_id,
        sdk_error_code,
    }
}

fn extract_sdk_error_details(details: &[serde_json::Value]) -> (Option<String>, Option<String>) {
    let mut request_id = None;
    let mut sdk_error_code = None;
    for detail in details {
        pull_sdk_fields(detail, &mut request_id, &mut sdk_error_code);
        if let Some(debug) = detail.get("debug") {
            pull_sdk_fields(debug, &mut request_id, &mut sdk_error_code);
        }
        if let Some(decoded) = decode_sdk_error_details_any(detail) {
            if request_id.is_none() {
                request_id = decoded.request_id.filter(|s| !s.is_empty());
            }
            if sdk_error_code.is_none() {
                sdk_error_code = format_sdk_error_code(decoded.sdk_error_code);
            }
            if request_id.is_some() && sdk_error_code.is_some() {
                break;
            }
        }
    }
    (request_id, sdk_error_code)
}

fn pull_sdk_fields(
    obj: &serde_json::Value,
    request_id: &mut Option<String>,
    sdk_error_code: &mut Option<String>,
) {
    if request_id.is_none() {
        *request_id = obj
            .get("requestId")
            .or_else(|| obj.get("request_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    if sdk_error_code.is_none() {
        *sdk_error_code = sdk_error_code_from_json(
            obj.get("sdkErrorCode")
                .or_else(|| obj.get("sdk_error_code")),
        );
    }
}

fn sdk_error_code_from_json(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value?;
    if let Some(s) = value.as_str().filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    if let Some(n) = value.as_i64() {
        return format_sdk_error_code(n as i32);
    }
    None
}

fn format_sdk_error_code(code: i32) -> Option<String> {
    if code == 0 {
        return None;
    }
    Some(
        crate::pb::SdkErrorCode::try_from(code)
            .map(|c| c.as_str_name().to_string())
            .unwrap_or_else(|_| code.to_string()),
    )
}

fn decode_sdk_error_details_any(detail: &serde_json::Value) -> Option<crate::pb::SdkErrorDetails> {
    let type_url = detail
        .get("type")
        .or_else(|| detail.get("typeUrl"))
        .or_else(|| detail.get("@type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !type_url.is_empty() && !type_url.contains("SdkErrorDetails") {
        return None;
    }
    let b64 = detail.get("value").and_then(|v| v.as_str())?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    prost::Message::decode(bytes.as_slice()).ok()
}

/// Incremental frame decoder used by unit tests and live streams.
pub fn split_frames(body: &[u8]) -> Result<Vec<(u8, Bytes)>, CursorSdkError> {
    let mut buf = BytesMut::from(body);
    let mut frames = Vec::new();
    while buf.len() >= 5 {
        let flags = buf[0];
        let len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
        if buf.len() < 5 + len {
            break;
        }
        let _ = buf.split_to(5);
        let payload = buf.split_to(len).freeze();
        frames.push((flags, payload));
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[derive(Clone, PartialEq, Message)]
    struct Tiny {
        #[prost(string, tag = "1")]
        text: String,
    }

    #[test]
    fn frame_round_trip() {
        let msg = Tiny { text: "hi".into() };
        let framed = encode_connect_frame(0, &msg.encode_to_vec());
        let frames = split_frames(&framed).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 0);
        let decoded = Tiny::decode(frames[0].1.clone()).unwrap();
        assert_eq!(decoded.text, "hi");
    }

    #[test]
    fn end_stream_error() {
        let err = serde_json::json!({"error":{"code":"unauthenticated","message":"no token"}});
        let framed = encode_connect_frame(0x02, err.to_string().as_bytes());
        let result = decode_connect_stream::<Tiny>("svc", "M", &framed);
        let err = result.unwrap_err();
        match err {
            CursorSdkError::Rpc { code, message, .. } => {
                assert_eq!(code, "unauthenticated");
                assert_eq!(message, "no token");
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn empty_keepalive_skipped() {
        let mut body = encode_connect_frame(0, &[]);
        let msg = Tiny { text: "ok".into() };
        body.extend_from_slice(&encode_connect_frame(0, &msg.encode_to_vec()));
        body.extend_from_slice(&encode_connect_frame(0x02, b"{}"));
        let msgs = decode_connect_stream::<Tiny>("svc", "M", &body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "ok");
    }

    #[test]
    fn incremental_decoder_reassembles_split_chunks() {
        let msg = Tiny {
            text: "chunked".into(),
        };
        let framed = encode_connect_frame(0, &msg.encode_to_vec());
        let mut dec = ConnectFrameDecoder::new();
        dec.push(&framed[..3]);
        assert!(dec.take_frames().is_empty());
        dec.push(&framed[3..]);
        let frames = dec.take_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(Tiny::decode(frames[0].1.clone()).unwrap().text, "chunked");
    }

    #[test]
    fn skip_undecodable_data_frame() {
        let mut body = encode_connect_frame(0, b"not-a-tiny-proto");
        let msg = Tiny {
            text: "kept".into(),
        };
        body.extend_from_slice(&encode_connect_frame(0, &msg.encode_to_vec()));
        body.extend_from_slice(&encode_connect_frame(0x02, b"{}"));
        let msgs = decode_connect_stream::<Tiny>("svc", "M", &body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "kept");
    }

    #[test]
    fn sdk_error_details_from_debug_json() {
        let err = serde_json::json!({
            "error": {
                "code": "unauthenticated",
                "message": "no token",
                "details": [{
                    "type": "sdk.v1.SdkErrorDetails",
                    "debug": {
                        "requestId": "req-abc",
                        "sdkErrorCode": "SDK_ERROR_CODE_UNAUTHORIZED"
                    }
                }]
            }
        });
        let framed = encode_connect_frame(0x02, err.to_string().as_bytes());
        let result = decode_connect_stream::<Tiny>("svc", "M", &framed).unwrap_err();
        match result {
            CursorSdkError::Rpc {
                code,
                request_id,
                sdk_error_code,
                message,
                ..
            } => {
                assert_eq!(code, "unauthenticated");
                assert_eq!(request_id.as_deref(), Some("req-abc"));
                assert_eq!(
                    sdk_error_code.as_deref(),
                    Some("SDK_ERROR_CODE_UNAUTHORIZED")
                );
                assert!(message.contains("request_id=req-abc"), "{message}");
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn sdk_error_details_from_proto_any() {
        use base64::Engine;
        use prost::Message;

        let details = crate::pb::SdkErrorDetails {
            request_id: Some("req-proto".into()),
            sdk_error_code: crate::pb::SdkErrorCode::AgentNotFound as i32,
            message: "gone".into(),
            help_url: None,
            provider: None,
            retry_after: None,
            rate_limit: None,
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(details.encode_to_vec());
        let err = serde_json::json!({
            "error": {
                "code": "not_found",
                "message": "missing",
                "details": [{
                    "type": "type.googleapis.com/sdk.v1.SdkErrorDetails",
                    "value": b64
                }]
            }
        });
        let framed = encode_connect_frame(0x02, err.to_string().as_bytes());
        let result = decode_connect_stream::<Tiny>("svc", "M", &framed).unwrap_err();
        match result {
            CursorSdkError::Rpc {
                request_id,
                sdk_error_code,
                ..
            } => {
                assert_eq!(request_id.as_deref(), Some("req-proto"));
                assert_eq!(
                    sdk_error_code.as_deref(),
                    Some("SDK_ERROR_CODE_AGENT_NOT_FOUND")
                );
            }
            other => panic!("{other}"),
        }
    }
}
