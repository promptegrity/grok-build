//! Cross-session peer messaging: inbox lifecycle for resident sessions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use agent_client_protocol as acp;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use xai_grok_peers::{
    InboxEnvelope, InboxHandle, PeerRecord, RepeatGuard, bind_inbox, default_name_from_cwd,
    format_inbound_prompt, inbox_path_for_session, register, sanitize_peer_name, unregister,
    update_name,
};

use crate::session::commands::SessionCommand;

static RUNTIMES: OnceLock<Mutex<HashMap<String, PeerRuntime>>> = OnceLock::new();

fn runtimes() -> &'static Mutex<HashMap<String, PeerRuntime>> {
    RUNTIMES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// How inbound peer envelopes are delivered to the session actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxMode {
    /// Inject as a Grok turn (`SessionCommand::Interject`).
    Interject,
    /// Forward to the pager Cursor client (no Grok turn).
    CursorProxy,
}

struct PeerRuntime {
    name: String,
    cursor_proxy: Arc<AtomicBool>,
    _inbox: InboxHandle,
    _bridge: tokio::task::JoinHandle<()>,
}

/// Switch inbox delivery for a live session. No-op if the session is not registered.
pub fn set_inbox_mode(session_id: &str, mode: InboxMode) {
    if let Some(rt) = runtimes().lock().get(session_id) {
        rt.cursor_proxy
            .store(matches!(mode, InboxMode::CursorProxy), Ordering::Relaxed);
    }
}

/// Current inbox mode, if the session is registered.
pub fn inbox_mode(session_id: &str) -> Option<InboxMode> {
    runtimes().lock().get(session_id).map(|rt| {
        if rt.cursor_proxy.load(Ordering::Relaxed) {
            InboxMode::CursorProxy
        } else {
            InboxMode::Interject
        }
    })
}

/// Choose the session command for an inbound envelope.
pub fn command_for_envelope(mode: InboxMode, env: &InboxEnvelope) -> SessionCommand {
    match mode {
        InboxMode::CursorProxy => SessionCommand::CursorProxyInbound {
            text: env.message.clone(),
            reply_to: env
                .reply_to
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| env.from_name.clone()),
            from_name: env.from_name.clone(),
        },
        InboxMode::Interject => {
            let text = format_inbound_prompt(env);
            let id = Some(format!("peer-msg-{}", uuid::Uuid::new_v4()));
            SessionCommand::Interject {
                text,
                id,
                images: Vec::new(),
            }
        }
    }
}

/// Start peer messaging for a resident session.
///
/// Binds an inbox socket, registers in `~/.grok/peers/`, and forwards inbound
/// envelopes as [`SessionCommand::Interject`] prompts.
pub async fn start_for_session(
    session_id: &str,
    cwd: &str,
    preferred_name: Option<&str>,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
) -> Option<String> {
    stop_for_session(session_id);

    let preferred = preferred_name
        .map(sanitize_peer_name)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_name_from_cwd(cwd, session_id));

    let inbox_path = inbox_path_for_session(&xai_grok_config::grok_home(), session_id);
    let (deliver_tx, mut deliver_rx) = mpsc::unbounded_channel::<InboxEnvelope>();

    let inbox = match bind_inbox(inbox_path.clone(), deliver_tx).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, session_id, "failed to bind peer inbox");
            return None;
        }
    };

    let record = PeerRecord {
        session_id: session_id.to_string(),
        name: preferred,
        cwd: cwd.to_string(),
        pid: std::process::id(),
        inbox_path: inbox.path().display().to_string(),
        updated_at: chrono::Utc::now(),
        note: None,
    };

    let registered = match register(record) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, session_id, "failed to register peer");
            return None;
        }
    };

    let sid = session_id.to_string();
    let cursor_proxy = Arc::new(AtomicBool::new(false));
    let cursor_proxy_flag = Arc::clone(&cursor_proxy);
    let bridge = tokio::spawn(async move {
        let mut repeats = RepeatGuard::new(std::time::Duration::from_secs(2));
        while let Some(env) = deliver_rx.recv().await {
            let key = format!("{}:{}", env.from_session_id, env.message);
            if !repeats.accept(&key) {
                continue;
            }
            let mode = if cursor_proxy_flag.load(Ordering::Relaxed) {
                InboxMode::CursorProxy
            } else {
                InboxMode::Interject
            };
            if cmd_tx.send(command_for_envelope(mode, &env)).is_err() {
                tracing::debug!(session_id = %sid, "peer inbox: session command channel closed");
                break;
            }
        }
    });

    let name = registered.name.clone();
    runtimes().lock().insert(
        session_id.to_string(),
        PeerRuntime {
            name: name.clone(),
            cursor_proxy,
            _inbox: inbox,
            _bridge: bridge,
        },
    );
    Some(name)
}

/// Stop peer messaging and unregister the session.
pub fn stop_for_session(session_id: &str) {
    if let Some(rt) = runtimes().lock().remove(session_id) {
        rt._bridge.abort();
        drop(rt._inbox);
    }
    if let Err(e) = unregister(session_id) {
        tracing::debug!(error = %e, session_id, "peer unregister failed");
    }
}

/// Update the live peer name after `/rename`.
pub fn rename_for_session(session_id: &str, new_name: &str) -> Option<String> {
    match update_name(session_id, new_name) {
        Ok(Some(record)) => {
            if let Some(rt) = runtimes().lock().get_mut(session_id) {
                rt.name = record.name.clone();
            }
            Some(record.name)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, session_id, "peer rename failed");
            None
        }
    }
}

/// Current live peer name for a session, if registered.
pub fn live_name(session_id: &str) -> Option<String> {
    runtimes().lock().get(session_id).map(|rt| rt.name.clone())
}

/// Parse preferred session name from ACP `_meta.sessionName`.
pub fn preferred_name_from_meta(meta: Option<&acp::Meta>) -> Option<String> {
    let name = meta?
        .get("sessionName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let sanitized = sanitize_peer_name(name);
    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

/// Persist `--name` / `_meta.sessionName` as a manual session title.
pub async fn apply_startup_name_title(
    info: &crate::session::info::Info,
    persistence_tx: &mpsc::UnboundedSender<crate::session::persistence::PersistenceMsg>,
    name: &str,
) {
    let title = sanitize_peer_name(name);
    if title.is_empty() {
        return;
    }
    let storage = crate::session::storage::JsonlStorageAdapter::default();
    use crate::session::storage::StorageAdapter;
    if let Err(e) = storage.update_session_title(info, title.clone()).await {
        tracing::warn!(error = %e, "failed to apply --name as session title");
        return;
    }
    let _ =
        persistence_tx.send(crate::session::persistence::PersistenceMsg::ManualTitleRenamed(title));
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_peers::InboxEnvelope;

    fn env() -> InboxEnvelope {
        InboxEnvelope {
            from_name: "api".into(),
            from_session_id: "s1".into(),
            message: "ship it".into(),
            reply_to: Some("api".into()),
        }
    }

    #[test]
    fn cursor_proxy_keeps_structured_envelope() {
        match command_for_envelope(InboxMode::CursorProxy, &env()) {
            SessionCommand::CursorProxyInbound {
                text,
                reply_to,
                from_name,
            } => {
                assert_eq!(text, "ship it");
                assert_eq!(reply_to, "api");
                assert_eq!(from_name, "api");
            }
            _ => panic!("expected CursorProxyInbound"),
        }
    }

    #[test]
    fn interject_mode_formats_prompt() {
        match command_for_envelope(InboxMode::Interject, &env()) {
            SessionCommand::Interject { text, .. } => {
                assert!(text.contains("api"));
                assert!(text.contains("ship it"));
            }
            _ => panic!("expected Interject"),
        }
    }
}
