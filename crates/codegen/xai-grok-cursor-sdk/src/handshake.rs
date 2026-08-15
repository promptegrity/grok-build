use serde::Deserialize;

use crate::error::CursorSdkError;

/// Literal prefix of the bridge ready line, including the trailing space.
pub const READY_LINE_PREFIX: &str = "cursor-sdk-bridge ready ";

/// Parsed ready-line discovery payload (`schemaVersion == 1`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyInfo {
    pub schema_version: u32,
    #[serde(default)]
    pub server_version: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    pub transport: String,
    pub protocol: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub auth_token_file: Option<String>,
    #[serde(default)]
    pub auth_token: Option<String>,
}

impl ReadyInfo {
    pub fn endpoint_url(&self) -> Result<String, CursorSdkError> {
        if let Some(url) = self.url.as_deref().filter(|s| !s.is_empty()) {
            return Ok(url.trim_end_matches('/').to_string());
        }
        let host = self.host.as_deref().unwrap_or("127.0.0.1");
        let port = self
            .port
            .ok_or_else(|| CursorSdkError::Handshake("ready line missing url and port".into()))?;
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        Ok(format!("http://{host}:{port}"))
    }
}

/// Parse a stderr line. `None` if it is not the discovery line.
pub fn parse_ready_line(line: &str) -> Result<Option<ReadyInfo>, CursorSdkError> {
    let Some(json) = line.strip_prefix(READY_LINE_PREFIX) else {
        return Ok(None);
    };
    let info: ReadyInfo = serde_json::from_str(json)
        .map_err(|e| CursorSdkError::Handshake(format!("invalid ready JSON: {e}")))?;
    if info.schema_version != 1 {
        return Err(CursorSdkError::Handshake(format!(
            "unsupported ready schemaVersion {}",
            info.schema_version
        )));
    }
    if info.transport != "tcp" {
        return Err(CursorSdkError::Handshake(format!(
            "unsupported transport {}",
            info.transport
        )));
    }
    if info.protocol != "connect" {
        return Err(CursorSdkError::Handshake(format!(
            "unsupported protocol {}",
            info.protocol
        )));
    }
    Ok(Some(info))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"cursor-sdk-bridge ready {"schemaVersion":1,"serverVersion":"1.0.0","pid":12345,"transport":"tcp","protocol":"connect","host":"127.0.0.1","port":49152,"url":"http://127.0.0.1:49152","authTokenFile":"/tmp/auth-token"}"#;

    #[test]
    fn parses_valid_ready_line() {
        let info = parse_ready_line(SAMPLE).unwrap().unwrap();
        assert_eq!(info.schema_version, 1);
        assert_eq!(info.transport, "tcp");
        assert_eq!(info.protocol, "connect");
        assert_eq!(info.endpoint_url().unwrap(), "http://127.0.0.1:49152");
        assert_eq!(info.auth_token_file.as_deref(), Some("/tmp/auth-token"));
    }

    #[test]
    fn ignores_unknown_fields() {
        let line = r#"cursor-sdk-bridge ready {"schemaVersion":1,"transport":"tcp","protocol":"connect","url":"http://127.0.0.1:1","futureField":true}"#;
        let info = parse_ready_line(line).unwrap().unwrap();
        assert_eq!(info.endpoint_url().unwrap(), "http://127.0.0.1:1");
    }

    #[test]
    fn rejects_bad_schema() {
        let line = r#"cursor-sdk-bridge ready {"schemaVersion":2,"transport":"tcp","protocol":"connect","url":"http://127.0.0.1:1"}"#;
        let err = parse_ready_line(line).unwrap_err();
        assert!(err.to_string().contains("schemaVersion"));
    }

    #[test]
    fn non_ready_is_none() {
        assert!(
            parse_ready_line("listening on 127.0.0.1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn host_port_fallback() {
        let line = r#"cursor-sdk-bridge ready {"schemaVersion":1,"transport":"tcp","protocol":"connect","host":"127.0.0.1","port":9}"#;
        let info = parse_ready_line(line).unwrap().unwrap();
        assert_eq!(info.endpoint_url().unwrap(), "http://127.0.0.1:9");
    }
}
