use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::discover::discover_bridge_bin;
use crate::error::CursorSdkError;
use crate::handshake::{ReadyInfo, parse_ready_line};
use crate::pb::{GetVersionRequest, PingRequest, ShutdownRequest};
use crate::transport;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

/// Live sidecar process + Connect endpoint.
pub struct BridgeHandle {
    pub url: String,
    pub bearer: String,
    child: Arc<Mutex<Option<Child>>>,
    http: reqwest::Client,
}

impl BridgeHandle {
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub async fn ping(&self) -> Result<String, CursorSdkError> {
        let resp: crate::pb::PingResponse = transport::unary(
            &self.http,
            &self.url,
            "sdk.v1.SdkBridgeControlService",
            "Ping",
            &self.bearer,
            &PingRequest {},
        )
        .await?;
        Ok(resp.message)
    }

    pub async fn get_version(&self) -> Result<crate::pb::GetVersionResponse, CursorSdkError> {
        transport::unary(
            &self.http,
            &self.url,
            "sdk.v1.SdkBridgeControlService",
            "GetVersion",
            &self.bearer,
            &GetVersionRequest {},
        )
        .await
    }

    pub async fn shutdown(&self) -> Result<(), CursorSdkError> {
        let _ = transport::unary::<_, crate::pb::ShutdownResponse>(
            &self.http,
            &self.url,
            "sdk.v1.SdkBridgeControlService",
            "Shutdown",
            &self.bearer,
            &ShutdownRequest { grace_seconds: 1 },
        )
        .await;
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            let wait = tokio::time::timeout(SHUTDOWN_WAIT, child.wait()).await;
            if wait.is_err() {
                let _ = child.kill().await;
            }
        }
        Ok(())
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.try_lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.start_kill();
        }
    }
}

impl Drop for BridgeManager {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.inner.try_lock()
            && let Some(handle) = guard.take()
        {
            drop(handle);
        }
    }
}

/// Lazy one-bridge-per-process manager.
pub struct BridgeManager {
    inner: Mutex<Option<Arc<BridgeHandle>>>,
    workspace: PathBuf,
    api_key: String,
}

impl BridgeManager {
    pub fn new(workspace: PathBuf, api_key: String) -> Self {
        Self {
            inner: Mutex::new(None),
            workspace,
            api_key,
        }
    }

    pub async fn handle(&self) -> Result<Arc<BridgeHandle>, CursorSdkError> {
        let mut guard = self.inner.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.clone());
        }
        let handle =
            Arc::new(spawn_bridge(&discover_bridge_bin()?, &self.workspace, &self.api_key).await?);
        *guard = Some(handle.clone());
        Ok(handle)
    }

    pub async fn close(&self) {
        let mut guard = self.inner.lock().await;
        if let Some(handle) = guard.take() {
            let _ = handle.shutdown().await;
        }
    }
}

pub async fn spawn_bridge(
    bin: &Path,
    workspace: &Path,
    api_key: &str,
) -> Result<BridgeHandle, CursorSdkError> {
    let mut cmd = Command::new(bin);
    cmd.arg("--workspace")
        .arg(workspace)
        .env("CURSOR_API_KEY", api_key)
        .env("CURSOR_SDK_CLIENT_LANGUAGE", "rust")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    xai_tty_utils::detach_command(&mut cmd);

    #[allow(clippy::disallowed_methods)] // sidecar owned by BridgeHandle; killed on Drop/shutdown
    let mut child = cmd.spawn().map_err(|e| {
        CursorSdkError::Handshake(format!("failed to spawn {}: {e}", bin.display()))
    })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CursorSdkError::Handshake("bridge stderr not piped".into()))?;

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tx = Some(ready_tx);
        let mut captured = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            match parse_ready_line(&line) {
                Ok(Some(info)) => {
                    if let Some(sender) = tx.take() {
                        let _ = sender.send(Ok(info));
                    }
                }
                Ok(None) => {
                    if captured.len() < 8_192 {
                        captured.push_str(&line);
                        captured.push('\n');
                    }
                }
                Err(e) => {
                    if let Some(sender) = tx.take() {
                        let _ = sender.send(Err(e));
                    }
                }
            }
        }
        if let Some(sender) = tx {
            let _ = sender.send(Err(CursorSdkError::Handshake(format!(
                "bridge exited before ready line. stderr:\n{captured}"
            ))));
        }
    });

    let info: ReadyInfo = match tokio::time::timeout(STARTUP_TIMEOUT, ready_rx).await {
        Ok(Ok(Ok(info))) => info,
        Ok(Ok(Err(e))) => return Err(e),
        Ok(Err(_)) => {
            return Err(CursorSdkError::Handshake(
                "ready-line channel closed".into(),
            ));
        }
        Err(_) => {
            let _ = child.kill().await;
            return Err(CursorSdkError::Handshake(
                "timed out waiting for cursor-sdk-bridge ready line".into(),
            ));
        }
    };

    let bearer = read_bearer(&info)?;
    let url = info.endpoint_url()?;
    let http = reqwest::Client::builder()
        .http1_only()
        .no_proxy()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(CursorSdkError::from)?;

    let handle = BridgeHandle {
        url,
        bearer,
        child: Arc::new(Mutex::new(Some(child))),
        http,
    };
    if let Err(e) = verify_live_bridge(&handle).await {
        let _ = handle.shutdown().await;
        return Err(e);
    }
    Ok(handle)
}

/// Ping then GetVersion; require `protocol_version == "sdk.v1"`.
async fn verify_live_bridge(handle: &BridgeHandle) -> Result<(), CursorSdkError> {
    handle
        .ping()
        .await
        .map_err(|e| CursorSdkError::Handshake(format!("Ping after spawn failed: {e}")))?;
    let version = handle
        .get_version()
        .await
        .map_err(|e| CursorSdkError::Handshake(format!("GetVersion after spawn failed: {e}")))?;
    check_protocol_version(&version.protocol_version)
}

pub(crate) fn check_protocol_version(protocol_version: &str) -> Result<(), CursorSdkError> {
    if protocol_version != "sdk.v1" {
        return Err(CursorSdkError::Handshake(format!(
            "unsupported protocol_version {protocol_version:?} (expected sdk.v1)"
        )));
    }
    Ok(())
}

fn read_bearer(info: &ReadyInfo) -> Result<String, CursorSdkError> {
    if let Some(token) = info
        .auth_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(token.to_string());
    }
    let path = info
        .auth_token_file
        .as_deref()
        .ok_or_else(|| CursorSdkError::Handshake("ready line missing authTokenFile".into()))?;
    let raw = std::fs::read_to_string(path).map_err(|e| {
        CursorSdkError::Handshake(format!("failed to read auth token file {path}: {e}"))
    })?;
    let token = raw.trim();
    if token.is_empty() {
        return Err(CursorSdkError::Handshake("auth token file is empty".into()));
    }
    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::check_protocol_version;

    #[test]
    fn protocol_version_sdk_v1_ok() {
        check_protocol_version("sdk.v1").unwrap();
    }

    #[test]
    fn protocol_version_rejects_other() {
        let err = check_protocol_version("sdk.v2").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sdk.v2"), "{msg}");
        assert!(msg.contains("sdk.v1"), "{msg}");
    }
}
