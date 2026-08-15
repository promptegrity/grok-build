use bytes::{Buf, Bytes, BytesMut};
use prost::Message;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

use crate::error::CursorSdkError;

const CONNECT_PROTO_UNARY: &str = "application/proto";
const CONNECT_PROTO_STREAM: &str = "application/connect+proto";
const CONNECT_PROTOCOL_VERSION: &str = "1";

/// Unary Connect POST with a raw protobuf body (`application/proto`).
pub async fn unary<Req: Message, Resp: Message + Default>(
    http: &reqwest::Client,
    base_url: &str,
    service: &str,
    method: &str,
    bearer: &str,
    request: &Req,
) -> Result<Resp, CursorSdkError> {
    let url = format!("{base_url}/{service}/{method}");
    let body = request.encode_to_vec();
    let response = http
        .post(&url)
        .header(AUTHORIZATION, format!("Bearer {bearer}"))
        .header(CONTENT_TYPE, CONNECT_PROTO_UNARY)
        .header("Connect-Protocol-Version", CONNECT_PROTOCOL_VERSION)
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

/// Server-stream Connect POST (`application/connect+proto`).
pub async fn server_stream<Req: Message, Resp: Message + Default>(
    http: &reqwest::Client,
    base_url: &str,
    service: &str,
    method: &str,
    bearer: &str,
    request: &Req,
) -> Result<Vec<Resp>, CursorSdkError> {
    let url = format!("{base_url}/{service}/{method}");
    let envelope = encode_connect_frame(0, &request.encode_to_vec());
    let response = http
        .post(&url)
        .header(AUTHORIZATION, format!("Bearer {bearer}"))
        .header(CONTENT_TYPE, CONNECT_PROTO_STREAM)
        .header("Connect-Protocol-Version", CONNECT_PROTOCOL_VERSION)
        .body(envelope)
        .send()
        .await?;

    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        return Err(connect_error_from_body(service, method, &bytes));
    }
    decode_connect_stream::<Resp>(service, method, &bytes)
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
        messages.push(Resp::decode(payload)?);
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
    }
}

fn connect_error(service: &str, method: &str, err: &ConnectErrorBody) -> CursorSdkError {
    let request_id = err.details.iter().find_map(|d| {
        d.get("requestId")
            .or_else(|| d.get("request_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });
    CursorSdkError::Rpc {
        method: format!("{service}/{method}"),
        code: if err.code.is_empty() {
            "unknown".into()
        } else {
            err.code.clone()
        },
        message: err.message.clone(),
        request_id,
    }
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
        let msg = Tiny {
            text: "hi".into(),
        };
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
        let msg = Tiny {
            text: "ok".into(),
        };
        body.extend_from_slice(&encode_connect_frame(0, &msg.encode_to_vec()));
        body.extend_from_slice(&encode_connect_frame(0x02, b"{}"));
        let msgs = decode_connect_stream::<Tiny>("svc", "M", &body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "ok");
    }
}
