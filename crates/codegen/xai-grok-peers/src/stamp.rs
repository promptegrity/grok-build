//! Last-send stamp so a Cursor seat can skip auto-relay after MCP `send_message`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::PEERS_DIR;

/// Recorded after a successful MCP (or tool) send from a seat session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastSendStamp {
    pub to: String,
    pub to_name: String,
    pub to_session_id: String,
    pub at_unix_ms: i64,
}

fn safe_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn stamp_path_in(root: &Path, session_id: &str) -> PathBuf {
    root.join(PEERS_DIR)
        .join(format!("{}.last-send.json", safe_session_id(session_id)))
}

/// Current time in unix milliseconds.
pub fn unix_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Write a last-send stamp for `session_id` under grok home.
pub fn record_last_send(
    session_id: &str,
    to: &str,
    to_name: &str,
    to_session_id: &str,
) -> io::Result<()> {
    record_last_send_in(
        &xai_grok_config::grok_home(),
        session_id,
        to,
        to_name,
        to_session_id,
        unix_now_ms(),
    )
}

pub fn record_last_send_in(
    root: &Path,
    session_id: &str,
    to: &str,
    to_name: &str,
    to_session_id: &str,
    at_unix_ms: i64,
) -> io::Result<()> {
    if session_id.is_empty() {
        return Ok(());
    }
    let dir = root.join(PEERS_DIR);
    fs::create_dir_all(&dir)?;
    let stamp = LastSendStamp {
        to: to.to_string(),
        to_name: to_name.to_string(),
        to_session_id: to_session_id.to_string(),
        at_unix_ms,
    };
    let path = stamp_path_in(root, session_id);
    let tmp = path.with_extension("last-send.json.tmp");
    let json = serde_json::to_vec(&stamp).map_err(io::Error::other)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// If this session sent to `reply_to` at or after `not_before_ms`, consume the stamp.
pub fn consume_last_send_if_matches(session_id: &str, reply_to: &str, not_before_ms: i64) -> bool {
    consume_last_send_if_matches_in(
        &xai_grok_config::grok_home(),
        session_id,
        reply_to,
        not_before_ms,
    )
}

pub fn consume_last_send_if_matches_in(
    root: &Path,
    session_id: &str,
    reply_to: &str,
    not_before_ms: i64,
) -> bool {
    if session_id.is_empty() || reply_to.is_empty() {
        return false;
    }
    let path = stamp_path_in(root, session_id);
    let Ok(bytes) = fs::read(&path) else {
        return false;
    };
    let Ok(stamp) = serde_json::from_slice::<LastSendStamp>(&bytes) else {
        let _ = fs::remove_file(&path);
        return false;
    };
    let matches = stamp.at_unix_ms >= not_before_ms
        && (stamp.to.eq_ignore_ascii_case(reply_to)
            || stamp.to_name.eq_ignore_ascii_case(reply_to)
            || stamp.to_session_id == reply_to);
    if matches {
        let _ = fs::remove_file(&path);
    }
    matches
}

/// True when a non-empty reply should be auto-relayed (no matching MCP send).
pub fn should_fallback_relay(reply_to: Option<&str>, reply: &str, stamp_matched: bool) -> bool {
    reply_to.is_some_and(|s| !s.is_empty()) && !reply.trim().is_empty() && !stamp_matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn stamp_roundtrip_consume() {
        let dir = tempdir().unwrap();
        record_last_send_in(dir.path(), "sess-1", "api", "api", "s-api", 100).unwrap();
        assert!(!consume_last_send_if_matches_in(
            dir.path(),
            "sess-1",
            "api",
            200
        ));
        assert!(consume_last_send_if_matches_in(
            dir.path(),
            "sess-1",
            "api",
            50
        ));
        assert!(!consume_last_send_if_matches_in(
            dir.path(),
            "sess-1",
            "api",
            50
        ));
    }

    #[test]
    fn fallback_relay_rules() {
        assert!(should_fallback_relay(Some("api"), "hi", false));
        assert!(!should_fallback_relay(Some("api"), "hi", true));
        assert!(!should_fallback_relay(Some("api"), "  ", false));
        assert!(!should_fallback_relay(None, "hi", false));
        assert!(!should_fallback_relay(Some(""), "hi", false));
    }
}
