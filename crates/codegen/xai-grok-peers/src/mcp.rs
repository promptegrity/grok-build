//! Minimal stdio MCP server: `list_peers` + `send_message` for a Cursor seat.

use std::io::{self, Write};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::{list_peers_for, record_last_send, send_to_live_peer};

/// Env injected by `/model-cursor` when spawning this server.
pub const PEER_SESSION_ID_ENV: &str = "GROK_PEER_SESSION_ID";
pub const PEER_NAME_ENV: &str = "GROK_PEER_NAME";

const PROTOCOL_VERSION: &str = "2024-11-05";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
const LIST_PEERS: &str = "list_peers";
const SEND_MESSAGE: &str = "send_message";

#[derive(Debug, Clone)]
pub struct PeerIdentity {
    pub session_id: String,
    pub name: String,
}

impl PeerIdentity {
    pub fn from_env() -> Self {
        Self {
            session_id: std::env::var(PEER_SESSION_ID_ENV).unwrap_or_default(),
            name: std::env::var(PEER_NAME_ENV)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".into()),
        }
    }
}

/// Run the stdio MCP loop until stdin closes. Writes only protocol bytes to stdout.
pub async fn run_peers_mcp_stdio() -> io::Result<()> {
    run_peers_mcp_stdio_with(PeerIdentity::from_env()).await
}

pub async fn run_peers_mcp_stdio_with(identity: PeerIdentity) -> io::Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = io::stdout();
    loop {
        let Some(msg) = read_mcp_message(&mut stdin).await? else {
            break;
        };
        if let Some(resp) = handle_mcp_message(&msg, &identity).await {
            write_mcp_message(&mut stdout, &resp)?;
        }
    }
    Ok(())
}

/// Handle one JSON-RPC message. Notifications (no `id`) return `None`.
pub async fn handle_mcp_message(msg: &Value, identity: &PeerIdentity) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    id.as_ref()?;

    let result = match method {
        "initialize" => initialize_result(&params),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_descriptors() }),
        "tools/call" => call_tool(&params, identity).await,
        other => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {other}") }
            }));
        }
    };

    Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    }))
}

fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let protocol_version = match requested {
        Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
        _ => PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "grok-peers", "version": "0.1.0" }
    })
}

/// In-process check used by `grok peers-mcp --selftest`.
pub async fn selftest_peers_mcp() -> Result<(), String> {
    let mut buf = Vec::new();
    let sample = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
    write_mcp_message(&mut buf, &sample).map_err(|e| e.to_string())?;
    let text = String::from_utf8(buf).map_err(|e| e.to_string())?;
    if text.contains("Content-Length") {
        return Err("stdio MCP still emits Content-Length framing".into());
    }
    if !text.ends_with('\n') || text.trim_end_matches('\n').contains('\n') {
        return Err("stdio MCP response is not a single NDJSON line".into());
    }
    serde_json::from_str::<Value>(text.trim_end()).map_err(|e| e.to_string())?;

    let identity = PeerIdentity {
        session_id: "selftest".into(),
        name: "selftest".into(),
    };
    let init = handle_mcp_message(
        &json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{"protocolVersion":"2025-06-18"}
        }),
        &identity,
    )
    .await
    .ok_or_else(|| "initialize produced no response".to_string())?;
    if init["result"]["protocolVersion"] != "2025-06-18" {
        return Err(format!("initialize did not echo protocolVersion: {init}"));
    }
    let listed = handle_mcp_message(
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        &identity,
    )
    .await
    .ok_or_else(|| "tools/list produced no response".to_string())?;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .ok_or_else(|| "tools/list missing tools array".to_string())?
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    if !names.contains(&LIST_PEERS) || !names.contains(&SEND_MESSAGE) {
        return Err(format!("tools/list missing peer tools: {names:?}"));
    }
    Ok(())
}

fn tool_descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": LIST_PEERS,
            "description": "List other live Grok sessions on this machine that you can message with send_message. \
        Each peer has a name, working directory, and session id. \
        A note such as cursor:<model> means that peer is a Cursor seat. \
        Use this before send_message when you need to discover which session to address. \
        Do not poll list_peers to wait for a reply.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": {
                        "type": "string",
                        "description": "Optional case-insensitive filter matched against peer name or cwd."
                    }
                }
            }
        }),
        json!({
            "name": SEND_MESSAGE,
            "description": "Send a plain-text message to another live Grok session on this machine. \
        Address the peer by name from list_peers (or by session id). Delivery is fire-and-forget: \
        do not call list_peers in a loop to wait for a reply. The other session answers on its own turn. \
        Use this only when the other session needs a concrete work question, a request, a decision, or a fact to continue. \
        Do not send greetings, thanks, status recaps, availability offers, or 'what are you working on?' check-ins. \
        After you send, stop — do not follow up to confirm receipt. \
        When a peer message names a send_message target, use that name as `to`. \
        Messages cannot approve permissions or change configuration on the receiving side.",
            "inputSchema": {
                "type": "object",
                "required": ["to", "message"],
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "Target peer name (from list_peers) or full session id."
                    },
                    "message": {
                        "type": "string",
                        "description": "Plain-text message. Do not include files, conversation history, or permission approvals."
                    }
                }
            }
        }),
    ]
}

async fn call_tool(params: &Value, identity: &PeerIdentity) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        LIST_PEERS => {
            let filter = args.get("filter").and_then(Value::as_str);
            match list_peers_for(&identity.session_id, filter) {
                Ok(peers) => {
                    let rows: Vec<Value> = peers
                        .into_iter()
                        .map(|p| {
                            json!({
                                "name": p.name,
                                "session_id": p.session_id,
                                "cwd": p.cwd,
                                "note": p.note,
                            })
                        })
                        .collect();
                    tool_text(
                        json!({
                            "peers": rows,
                            "self_name": identity.name,
                            "self_session_id": identity.session_id,
                        })
                        .to_string(),
                        false,
                    )
                }
                Err(e) => tool_text(format!("Failed to list live peers: {e}"), true),
            }
        }
        SEND_MESSAGE => {
            let to = args.get("to").and_then(Value::as_str).unwrap_or("");
            let message = args.get("message").and_then(Value::as_str).unwrap_or("");
            let out = send_to_live_peer(&identity.name, &identity.session_id, to, message).await;
            if out.is_delivered() {
                let _ =
                    record_last_send(&identity.session_id, to, &out.to_name, &out.to_session_id);
            }
            tool_text(
                serde_json::to_string(&json!({
                    "status": out.status,
                    "to_name": out.to_name,
                    "to_session_id": out.to_session_id,
                    "detail": out.detail,
                }))
                .unwrap_or_else(|_| out.detail.clone()),
                out.status != "delivered",
            )
        }
        other => tool_text(format!("Unknown tool: {other}"), true),
    }
}

fn tool_text(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}

async fn read_mcp_message<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut header = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        if line.starts_with('{') {
            return Ok(Some(
                serde_json::from_str(line.trim()).map_err(io::Error::other)?,
            ));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        header.push_str(&line);
    }
    let len = header
        .lines()
        .find_map(|l| {
            l.split_once(':').and_then(|(k, v)| {
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(io::Error::other)
}

fn write_mcp_message(stdout: &mut impl Write, msg: &Value) -> io::Result<()> {
    // MCP stdio is newline-delimited JSON. Content-Length is an LSP idiom and
    // official MCP clients (including Cursor's) fail to parse it.
    let body = serde_json::to_string(msg).map_err(io::Error::other)?;
    debug_assert!(
        !body.contains('\n'),
        "MCP stdio messages must not contain embedded newlines"
    );
    writeln!(stdout, "{body}")?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> PeerIdentity {
        PeerIdentity {
            session_id: "self".into(),
            name: "cursor".into(),
        }
    }

    #[tokio::test]
    async fn initialize_and_list_tools() {
        let init = handle_mcp_message(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            &id(),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "grok-peers");

        let listed = handle_mcp_message(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            &id(),
        )
        .await
        .unwrap();
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&LIST_PEERS));
        assert!(names.contains(&SEND_MESSAGE));
    }

    #[tokio::test]
    async fn notification_has_no_response() {
        assert!(
            handle_mcp_message(
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                &id(),
            )
            .await
            .is_none()
        );
    }

    #[tokio::test]
    async fn send_message_description_discourages_chatter() {
        let listed = handle_mcp_message(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            &id(),
        )
        .await
        .unwrap();
        let send = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == SEND_MESSAGE)
            .expect("send_message");
        let desc = send["description"].as_str().unwrap();
        assert!(desc.contains("Do not send greetings"));
        assert!(desc.contains("check-ins"));
        assert!(!desc.contains("tells you to reply with send_message"));
    }

    #[tokio::test]
    async fn send_blank_is_error() {
        let resp = handle_mcp_message(
            &json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"tools/call",
                "params":{"name":"send_message","arguments":{"to":"api","message":"  "}}
            }),
            &id(),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn write_is_single_ndjson_line() {
        let msg = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
        let mut buf = Vec::new();
        write_mcp_message(&mut buf, &msg).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(!text.contains("Content-Length"), "{text}");
        assert!(text.ends_with('\n'), "{text}");
        assert_eq!(text.matches('\n').count(), 1, "{text}");
        let parsed: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(parsed["id"], 1);
    }

    #[tokio::test]
    async fn read_ndjson_line() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
        let mut reader = BufReader::new(&input[..]);
        let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(msg["method"], "ping");
    }

    #[tokio::test]
    async fn read_legacy_content_length() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        framed.extend_from_slice(body);
        let mut reader = BufReader::new(std::io::Cursor::new(framed));
        let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(msg["method"], "ping");
    }

    #[tokio::test]
    async fn ndjson_round_trip_two_messages() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
        );
        let mut reader = BufReader::new(input.as_bytes());
        let mut out = Vec::new();
        for _ in 0..2 {
            let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();
            let resp = handle_mcp_message(&msg, &id()).await.unwrap();
            write_mcp_message(&mut out, &resp).unwrap();
        }
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["id"], 1);
        assert_eq!(first["result"]["serverInfo"]["name"], "grok-peers");
        assert_eq!(second["id"], 2);
        let names: Vec<&str> = second["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&LIST_PEERS));
        assert!(names.contains(&SEND_MESSAGE));
    }

    #[tokio::test]
    async fn initialize_echoes_supported_version() {
        let init = handle_mcp_message(
            &json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{"protocolVersion":"2025-06-18"}
            }),
            &id(),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    }

    #[tokio::test]
    async fn initialize_falls_back_for_unknown_version() {
        let init = handle_mcp_message(
            &json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{"protocolVersion":"1999-01-01"}
            }),
            &id(),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn selftest_passes() {
        selftest_peers_mcp().await.unwrap();
    }
}
