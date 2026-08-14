//! Cross-session peer messaging: inbox lifecycle for resident sessions.

use std::collections::HashMap;
use std::sync::OnceLock;

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

struct PeerRuntime {
    name: String,
    _inbox: InboxHandle,
    _bridge: tokio::task::JoinHandle<()>,
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
    };

    let registered = match register(record) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, session_id, "failed to register peer");
            return None;
        }
    };

    let sid = session_id.to_string();
    let bridge = tokio::spawn(async move {
        let mut repeats = RepeatGuard::new(std::time::Duration::from_secs(2));
        while let Some(env) = deliver_rx.recv().await {
            let key = format!("{}:{}", env.from_session_id, env.message);
            if !repeats.accept(&key) {
                continue;
            }
            let text = format_inbound_prompt(&env);
            let id = Some(format!("peer-msg-{}", uuid::Uuid::new_v4()));
            if cmd_tx
                .send(SessionCommand::Interject {
                    text,
                    id,
                    images: Vec::new(),
                })
                .is_err()
            {
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
    runtimes()
        .lock()
        .get(session_id)
        .map(|rt| rt.name.clone())
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
    let _ = persistence_tx.send(crate::session::persistence::PersistenceMsg::ManualTitleRenamed(
        title,
    ));
}
