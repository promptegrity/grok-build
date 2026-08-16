//! Resolve a live peer and deliver a plain-text inbox message.

use crate::{PeerRecord, list_live, send_plain_message};

/// Max accepted message size (matches the Grok `send_message` tool).
pub const MAX_PEER_MESSAGE_LEN: usize = 32_768;

/// Outcome of [`send_to_live_peer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSendOutput {
    pub status: String,
    pub to_name: String,
    pub to_session_id: String,
    pub detail: String,
}

impl PeerSendOutput {
    pub fn delivered(to_name: String, to_session_id: String) -> Self {
        let detail = format!("Message delivered to peer '{to_name}'.");
        Self {
            status: "delivered".into(),
            to_name,
            to_session_id,
            detail,
        }
    }

    pub fn is_delivered(&self) -> bool {
        self.status == "delivered"
    }
}

/// Validate, resolve `to` against live peers, and deliver.
pub async fn send_to_live_peer(
    from_name: &str,
    from_session_id: &str,
    to: &str,
    message: &str,
) -> PeerSendOutput {
    let message = message.trim();
    if message.is_empty() {
        return PeerSendOutput {
            status: "unreachable".into(),
            to_name: to.to_string(),
            to_session_id: String::new(),
            detail: "message must not be blank".into(),
        };
    }
    if message.len() > MAX_PEER_MESSAGE_LEN {
        return PeerSendOutput {
            status: "unreachable".into(),
            to_name: to.to_string(),
            to_session_id: String::new(),
            detail: "message exceeds 32 KiB limit".into(),
        };
    }

    let target_key = to.trim();
    if target_key.is_empty() {
        return PeerSendOutput {
            status: "unreachable".into(),
            to_name: String::new(),
            to_session_id: String::new(),
            detail: "to must be a peer name or session id".into(),
        };
    }

    let live = match list_live() {
        Ok(v) => v,
        Err(e) => {
            return PeerSendOutput {
                status: "unreachable".into(),
                to_name: target_key.to_string(),
                to_session_id: String::new(),
                detail: format!("Failed to list live peers: {e}"),
            };
        }
    };

    let matches: Vec<&PeerRecord> = live
        .iter()
        .filter(|p| p.session_id != from_session_id)
        .filter(|p| {
            p.session_id == target_key
                || p.name.eq_ignore_ascii_case(target_key)
                || p.session_id.ends_with(target_key)
        })
        .collect();

    let target = match matches.as_slice() {
        [] => {
            return PeerSendOutput {
                status: "unreachable".into(),
                to_name: target_key.to_string(),
                to_session_id: String::new(),
                detail: format!(
                    "No live peer named '{target_key}'. Run list_peers to see reachable sessions."
                ),
            };
        }
        [one] => *one,
        many => {
            let names: Vec<String> = many
                .iter()
                .map(|p| {
                    format!(
                        "{} ({})",
                        p.name,
                        &p.session_id[..8.min(p.session_id.len())]
                    )
                })
                .collect();
            return PeerSendOutput {
                status: "ambiguous".into(),
                to_name: target_key.to_string(),
                to_session_id: String::new(),
                detail: format!(
                    "Multiple peers match '{target_key}': {}. Address by full session id.",
                    names.join(", ")
                ),
            };
        }
    };

    match send_plain_message(
        std::path::Path::new(&target.inbox_path),
        from_name,
        from_session_id,
        message,
        Some(from_name),
    )
    .await
    {
        Ok(()) => PeerSendOutput::delivered(target.name.clone(), target.session_id.clone()),
        Err(e) => PeerSendOutput {
            status: "unreachable".into(),
            to_name: target.name.clone(),
            to_session_id: target.session_id.clone(),
            detail: format!("Failed to deliver to '{}': {e}", target.name),
        },
    }
}

/// Filter live peers for `list_peers` (excludes `self_id`, optional substring).
pub fn list_peers_for(
    self_id: &str,
    filter: Option<&str>,
) -> Result<Vec<PeerRecord>, std::io::Error> {
    let filter = filter
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());
    Ok(list_live()?
        .into_iter()
        .filter(|p| p.session_id != self_id)
        .filter(|p| match &filter {
            None => true,
            Some(f) => {
                p.name.to_ascii_lowercase().contains(f) || p.cwd.to_ascii_lowercase().contains(f)
            }
        })
        .collect())
}
