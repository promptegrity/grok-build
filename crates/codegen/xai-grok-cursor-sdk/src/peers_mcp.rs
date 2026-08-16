//! Stdio `grok-peers` MCP attach for `/model-cursor` seats.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::pb::{mcp_server_config, McpServerConfig, StdioMcpServerConfig};

const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(8);
const LIST_PEERS: &str = "list_peers";
const SEND_MESSAGE: &str = "send_message";

/// Seat identity passed into the `grok peers-mcp` stdio server.
#[derive(Debug, Clone)]
pub struct PeersMcpIdentity {
    pub grok_bin: String,
    pub session_id: String,
    pub peer_name: String,
    pub cwd: String,
    pub grok_home: String,
}

impl PeersMcpIdentity {
    pub fn for_seat(
        session_id: impl Into<String>,
        peer_name: impl Into<String>,
        cwd: impl AsRef<Path>,
    ) -> Self {
        Self {
            grok_bin: crate::discover::resolve_grok_bin(),
            session_id: session_id.into(),
            peer_name: peer_name.into(),
            cwd: cwd.as_ref().to_string_lossy().into_owned(),
            grok_home: xai_grok_config::grok_home().to_string_lossy().into_owned(),
        }
    }
}

pub fn peers_mcp_servers(identity: &Option<PeersMcpIdentity>) -> HashMap<String, McpServerConfig> {
    let Some(id) = identity else {
        return HashMap::new();
    };
    let mut servers = HashMap::new();
    servers.insert(
        "grok-peers".to_string(),
        McpServerConfig {
            config: Some(mcp_server_config::Config::Stdio(StdioMcpServerConfig {
                command: id.grok_bin.clone(),
                args: vec!["peers-mcp".into()],
                env: stdio_child_env(id),
                cwd: id.cwd.clone(),
            })),
        },
    );
    servers
}

/// Environment for the MCP child, assuming replacement (not merge) semantics.
fn stdio_child_env(id: &PeersMcpIdentity) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("GROK_PEER_SESSION_ID".to_string(), id.session_id.clone());
    env.insert("GROK_PEER_NAME".to_string(), id.peer_name.clone());
    env.insert("GROK_HOME".to_string(), id.grok_home.clone());
    for key in [
        "HOME",
        "PATH",
        "USERPROFILE",
        "SYSTEMROOT",
        "APPDATA",
        "TMPDIR",
        "TEMP",
        "TMP",
        "LANG",
        "LC_ALL",
    ] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    env
}

/// Spawn `grok_bin peers-mcp` with the same env/cwd Cursor will use and require
/// `initialize` + `tools/list` over NDJSON before seating the agent.
pub async fn preflight_peers_mcp(identity: &PeersMcpIdentity) -> Result<(), String> {
    let servers = peers_mcp_servers(&Some(identity.clone()));
    let Some(cfg) = servers.get("grok-peers") else {
        return Err("internal: grok-peers stdio config missing".into());
    };
    let Some(mcp_server_config::Config::Stdio(stdio)) = cfg.config.as_ref() else {
        return Err("internal: grok-peers is not a stdio MCP server".into());
    };

    let mut cmd = tokio::process::Command::new(&stdio.command);
    cmd.args(&stdio.args)
        .env_clear()
        .envs(&stdio.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !stdio.cwd.is_empty() {
        cmd.current_dir(&stdio.cwd);
    }

    let scope = xai_tty_utils::ProcessScope::new();
    let (mut child, _group) = scope.spawn(cmd).map_err(|e| {
        format!(
            "failed to spawn peers-mcp at {} (cwd {}): {e}",
            stdio.command, stdio.cwd
        )
    })?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "peers-mcp child has no stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "peers-mcp child has no stdout".to_string())?;
    let mut reader = BufReader::new(stdout);

    let outcome = tokio::time::timeout(PREFLIGHT_TIMEOUT, async {
        handshake_initialize_and_list(&mut reader, &mut stdin).await
    })
    .await;

    let _ = child.start_kill();
    let _ = child.wait().await;

    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!(
            "peers-mcp at {} failed handshake: {e}",
            stdio.command
        )),
        Err(_) => Err(format!(
            "peers-mcp at {} did not answer initialize/tools/list within {}s",
            stdio.command,
            PREFLIGHT_TIMEOUT.as_secs()
        )),
    }
}

async fn handshake_initialize_and_list(
    reader: &mut (impl AsyncBufReadExt + Unpin),
    writer: &mut (impl AsyncWriteExt + Unpin),
) -> Result<(), String> {
    write_ndjson(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "grok", "version": "0" }
            }
        }),
    )
    .await?;
    let init = read_ndjson(reader).await?;
    if init.get("error").is_some() {
        return Err(format!("initialize error: {init}"));
    }
    if init["result"]["serverInfo"]["name"] != "grok-peers" {
        return Err(format!("initialize did not identify grok-peers: {init}"));
    }

    write_ndjson(
        writer,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await?;

    write_ndjson(
        writer,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await?;
    let listed = read_ndjson(reader).await?;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .ok_or_else(|| format!("tools/list missing tools: {listed}"))?
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    if !names.contains(&LIST_PEERS) || !names.contains(&SEND_MESSAGE) {
        return Err(format!("tools/list missing peer tools: {names:?}"));
    }
    Ok(())
}

async fn write_ndjson(
    writer: &mut (impl AsyncWriteExt + Unpin),
    msg: &Value,
) -> Result<(), String> {
    let body = serde_json::to_string(msg).map_err(|e| e.to_string())?;
    writer
        .write_all(body.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    writer.write_all(b"\n").await.map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())
}

async fn read_ndjson(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Result<Value, String> {
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("peers-mcp closed stdout before answering".into());
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err("peers-mcp wrote a blank line".into());
    }
    if trimmed.starts_with("Content-Length") {
        return Err("peers-mcp still uses Content-Length framing; Cursor requires NDJSON".into());
    }
    serde_json::from_str(trimmed)
        .map_err(|e| format!("peers-mcp wrote a non-JSON line ({e}): {trimmed}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_identity() -> PeersMcpIdentity {
        PeersMcpIdentity {
            grok_bin: "/usr/bin/grok".into(),
            session_id: "sess".into(),
            peer_name: "cursor".into(),
            cwd: "/tmp/ws".into(),
            grok_home: "/tmp/grok-home".into(),
        }
    }

    #[test]
    fn peers_mcp_stdio_config() {
        let servers = peers_mcp_servers(&Some(sample_identity()));
        let cfg = servers.get("grok-peers").expect("grok-peers server");
        match &cfg.config {
            Some(mcp_server_config::Config::Stdio(stdio)) => {
                assert_eq!(stdio.command, "/usr/bin/grok");
                assert_eq!(stdio.args, vec!["peers-mcp"]);
                assert_eq!(stdio.cwd, "/tmp/ws");
                assert_eq!(
                    stdio.env.get("GROK_PEER_SESSION_ID").map(String::as_str),
                    Some("sess")
                );
                assert_eq!(
                    stdio.env.get("GROK_PEER_NAME").map(String::as_str),
                    Some("cursor")
                );
                assert_eq!(
                    stdio.env.get("GROK_HOME").map(String::as_str),
                    Some("/tmp/grok-home")
                );
                if std::env::var_os("PATH").is_some() {
                    assert!(stdio.env.contains_key("PATH"));
                }
                if std::env::var_os("HOME").is_some() {
                    assert!(stdio.env.contains_key("HOME"));
                }
            }
            other => panic!("expected stdio config, got {other:?}"),
        }
        assert!(peers_mcp_servers(&None).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preflight_accepts_ndjson_child() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-grok");
        std::fs::write(
            &script,
            r#"#!/bin/sh
read line
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"grok-peers","version":"0.1.0"}}}'
read line
read line
echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"list_peers"},{"name":"send_message"}]}}'
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let identity = PeersMcpIdentity {
            grok_bin: script.to_string_lossy().into_owned(),
            session_id: "sess".into(),
            peer_name: "cursor".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            grok_home: dir.path().to_string_lossy().into_owned(),
        };
        preflight_peers_mcp(&identity).await.unwrap();
    }

    #[tokio::test]
    async fn preflight_against_pager_binary() {
        let Ok(bin) = std::env::var("PAGER_BINARY") else {
            return;
        };
        if bin.is_empty() || !std::path::Path::new(&bin).is_file() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        preflight_peers_mcp(&PeersMcpIdentity {
            grok_bin: bin,
            session_id: "sess".into(),
            peer_name: "probe".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            grok_home: dir.path().to_string_lossy().into_owned(),
        })
        .await
        .expect("PAGER_BINARY peers-mcp handshake");
    }

    #[tokio::test]
    async fn preflight_rejects_missing_binary() {
        let err = preflight_peers_mcp(&PeersMcpIdentity {
            grok_bin: "/no/such/grok-peers-mcp-bin".into(),
            session_id: "sess".into(),
            peer_name: "cursor".into(),
            cwd: "/tmp".into(),
            grok_home: "/tmp".into(),
        })
        .await
        .unwrap_err();
        assert!(
            err.contains("failed to spawn") || err.contains("/no/such/grok-peers-mcp-bin"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preflight_rejects_content_length_child() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-grok");
        std::fs::write(
            &script,
            r#"#!/bin/sh
body='{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"grok-peers"}}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let err = preflight_peers_mcp(&PeersMcpIdentity {
            grok_bin: script.to_string_lossy().into_owned(),
            session_id: "sess".into(),
            peer_name: "cursor".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            grok_home: dir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap_err();
        assert!(
            err.contains("Content-Length") || err.contains("non-JSON"),
            "{err}"
        );
    }
}
