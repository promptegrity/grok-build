//! Pager-owned Cursor client/proxy session mode (`/model-cursor`).

use std::cell::{Cell, RefCell};

use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::app::agent_view::AgentView;
use crate::app::app_view::AppView;

thread_local! {
    static MODELS: RefCell<Option<Vec<CursorModelChoice>>> = const { RefCell::new(None) };
    static FETCH_STARTED: Cell<bool> = const { Cell::new(false) };
}

/// Incremental Cursor run update shown in the TUI while `Send` is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorProxyProgress {
    Status(String),
    AssistantDelta(String),
}

/// One Cursor agent bound to a Grok session that forwards prompts.
#[derive(Debug, Clone)]
pub struct CursorClientSession {
    pub model_id: String,
    pub display_name: String,
    pub agent_id: String,
    pub inflight: bool,
    /// Latest status/step label for the turn-status spinner.
    pub activity: Option<String>,
    /// Streaming assistant block, if any text has arrived.
    pub stream_entry: Option<crate::scrollback::entry::EntryId>,
    pub streamed_text: String,
}

/// Prompt waiting because another Cursor send is in flight.
#[derive(Debug, Clone)]
pub struct CursorProxyQueued {
    pub text: String,
    pub reply_to: Option<String>,
}

/// A Cursor model offered by `/model-cursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorModelChoice {
    pub id: String,
    pub display_name: String,
    pub description: String,
}

/// Cached `ListModels` result, if a fetch has succeeded.
pub fn cached_models() -> Option<Vec<CursorModelChoice>> {
    MODELS.with(|slot| slot.borrow().clone())
}

/// Store models after a successful fetch.
pub fn set_cached_models(models: Vec<CursorModelChoice>) {
    MODELS.with(|slot| *slot.borrow_mut() = Some(models));
    FETCH_STARTED.with(|f| f.set(true));
}

/// Allow another fetch after a failure (or in tests).
pub fn reset_models_fetch() {
    FETCH_STARTED.with(|f| f.set(false));
}

#[cfg(test)]
pub fn clear_cached_models_for_test() {
    MODELS.with(|slot| *slot.borrow_mut() = None);
    FETCH_STARTED.with(|f| f.set(false));
}

/// Resolve a `/model-cursor` argument against the cache, or accept a raw id.
pub fn resolve_model_arg(args: &str) -> Option<(String, String)> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(models) = cached_models() {
        let needle = trimmed.to_ascii_lowercase();
        if let Some(m) = models.iter().find(|m| m.id.eq_ignore_ascii_case(trimmed)) {
            return Some((m.id.clone(), display_or_id(m)));
        }
        if let Some(m) = models
            .iter()
            .find(|m| m.display_name.to_ascii_lowercase() == needle)
        {
            return Some((m.id.clone(), display_or_id(m)));
        }
    }
    Some((trimmed.to_string(), trimmed.to_string()))
}

fn display_or_id(m: &CursorModelChoice) -> String {
    if m.display_name.trim().is_empty() {
        m.id.clone()
    } else {
        m.display_name.clone()
    }
}

/// True when the composer is on `/model-cursor` and we have not fetched yet.
pub fn should_prefetch_models(prompt: &str) -> bool {
    if cached_models().is_some() || FETCH_STARTED.with(|f| f.get()) {
        return false;
    }
    let trimmed = prompt.trim_start();
    let rest = trimmed
        .strip_prefix("/model-cursor")
        .or_else(|| trimmed.strip_prefix("/cursor-model"));
    rest.is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace))
}

/// Mark a fetch as started. Returns true if this caller should emit the effect.
pub fn mark_fetch_started() -> bool {
    FETCH_STARTED.with(|f| {
        let started = f.get();
        f.set(true);
        !started
    })
}

/// Status-bar / prompt label when Cursor client mode is active.
pub fn status_label(session: &CursorClientSession) -> String {
    format!("Cursor · {}", session.display_name)
}

/// Leave Cursor client mode. Returns true if a session was active.
pub fn clear_cursor_client(app: &mut AppView, agent_id: AgentId) -> bool {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return false;
    };
    clear_cursor_client_on_agent(agent)
}

/// Leave Cursor client mode on a specific agent view.
pub fn clear_cursor_client_on_agent(agent: &mut AgentView) -> bool {
    let Some(session) = agent.cursor_client.take() else {
        agent.cursor_proxy_queue.clear();
        return false;
    };
    agent.cursor_proxy_queue.clear();
    if let Some(sid) = agent.session.session_id.as_ref() {
        xai_grok_shell::peers::set_inbox_mode(
            sid.0.as_ref(),
            xai_grok_shell::peers::InboxMode::Interject,
        );
        let _ = xai_grok_peers::update_note(sid.0.as_ref(), None);
    }
    agent
        .scrollback
        .push_block(crate::scrollback::block::RenderBlock::system(format!(
            "Left Cursor client mode ({}). Prompts go to Grok again.",
            session.display_name
        )));
    true
}

/// Queue or start a Cursor proxy send. Echoes the user text first.
pub fn enqueue_or_send(
    agent: &mut AgentView,
    text: String,
    reply_to: Option<String>,
    display_text: Option<String>,
) -> Vec<Effect> {
    if agent.cursor_client.is_none() {
        return vec![];
    }
    let shown = display_text.unwrap_or_else(|| text.clone());
    agent
        .scrollback
        .push_block(crate::scrollback::block::RenderBlock::user_prompt(shown));
    if agent.cursor_client.as_ref().is_some_and(|c| c.inflight) {
        agent
            .cursor_proxy_queue
            .push_back(CursorProxyQueued { text, reply_to });
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::system(
                "Queued — waiting for the current Cursor run to finish.",
            ));
        return vec![];
    }
    begin_cursor_run(agent);
    vec![cursor_proxy_send_effect(agent, text, reply_to)]
}

/// Mark the session as a live Cursor turn (spinner + follow).
pub fn begin_cursor_run(agent: &mut AgentView) {
    agent.session.state = crate::app::agent::AgentState::TurnRunning;
    if let Some(client) = agent.cursor_client.as_mut() {
        client.inflight = true;
        client.activity = Some("working".into());
        client.stream_entry = None;
        client.streamed_text.clear();
    }
}

/// Apply a stream event to the live Cursor turn.
pub fn apply_progress(agent: &mut AgentView, event: CursorProxyProgress) {
    if !agent.cursor_client.as_ref().is_some_and(|c| c.inflight) {
        return;
    }
    match event {
        CursorProxyProgress::Status(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }
            if let Some(client) = agent.cursor_client.as_mut() {
                client.activity = Some(trimmed.to_string());
            }
        }
        CursorProxyProgress::AssistantDelta(delta) => {
            if delta.is_empty() {
                return;
            }
            let existing = agent.cursor_client.as_ref().and_then(|c| c.stream_entry);
            let id = existing.unwrap_or_else(|| {
                let id = agent.scrollback.start_streaming_agent();
                agent.scrollback.set_entry_running(id, true);
                id
            });
            if let Some(client) = agent.cursor_client.as_mut() {
                client.activity = Some("responding".into());
                client.stream_entry = Some(id);
                client.streamed_text.push_str(&delta);
            }
            agent.scrollback.push_chunk_to_agent(id, &delta);
        }
    }
}

/// End the live Cursor turn. Returns true if assistant text was already streamed.
pub fn finish_cursor_run(agent: &mut AgentView) -> bool {
    agent.session.state = crate::app::agent::AgentState::Idle;
    let Some(client) = agent.cursor_client.as_mut() else {
        return false;
    };
    client.activity = None;
    let streamed = !client.streamed_text.trim().is_empty();
    let stream_entry = client.stream_entry.take();
    client.streamed_text.clear();
    drop(client);
    if let Some(id) = stream_entry {
        agent.scrollback.set_entry_running(id, false);
    }
    streamed
}

/// Build the send effect from the current Cursor client session.
pub fn cursor_proxy_send_effect(
    agent: &AgentView,
    text: String,
    reply_to: Option<String>,
) -> Effect {
    let client = agent.cursor_client.as_ref().expect("cursor client active");
    let session_id = agent
        .session
        .session_id
        .as_ref()
        .map(|s| s.0.to_string())
        .unwrap_or_default();
    let from_name =
        xai_grok_shell::peers::live_name(&session_id).unwrap_or_else(|| "cursor-proxy".to_string());
    Effect::CursorProxySend {
        agent_id: agent.session.id,
        cwd: agent.session.cwd.clone(),
        cursor_agent_id: client.agent_id.clone(),
        text,
        reply_to,
        from_name,
        from_session_id: session_id,
    }
}

/// After a send completes, drain the next queued prompt if any.
pub fn drain_queued_send(agent: &mut AgentView) -> Vec<Effect> {
    let Some(next) = agent.cursor_proxy_queue.pop_front() else {
        if let Some(client) = agent.cursor_client.as_mut() {
            client.inflight = false;
        }
        return vec![];
    };
    begin_cursor_run(agent);
    vec![cursor_proxy_send_effect(agent, next.text, next.reply_to)]
}

/// Prefetch hook used from the agent draw path.
pub fn maybe_prefetch_models(agent: &mut AgentView) {
    if !should_prefetch_models(agent.prompt.text()) {
        return;
    }
    if !mark_fetch_started() {
        return;
    }
    agent.pending_effects.push(Effect::FetchCursorModels {
        agent_id: agent.session.id,
        cwd: agent.session.cwd.clone(),
    });
}

/// Bind a created Cursor agent and switch the peer inbox to proxy mode.
pub fn activate_on_agent(
    agent: &mut AgentView,
    model_id: String,
    display_name: String,
    cursor_agent_id: String,
) {
    agent.cursor_client = Some(CursorClientSession {
        model_id: model_id.clone(),
        display_name: display_name.clone(),
        agent_id: cursor_agent_id,
        inflight: false,
        activity: None,
        stream_entry: None,
        streamed_text: String::new(),
    });
    agent.cursor_proxy_queue.clear();
    if let Some(sid) = agent.session.session_id.as_ref() {
        xai_grok_shell::peers::set_inbox_mode(
            sid.0.as_ref(),
            xai_grok_shell::peers::InboxMode::CursorProxy,
        );
        let _ = xai_grok_peers::update_note(sid.0.as_ref(), Some(format!("cursor:{model_id}")));
    }
    agent
        .scrollback
        .push_block(crate::scrollback::block::RenderBlock::system(format!(
            "Cursor client mode: {display_name}. Prompts (and peer messages) go to Cursor. \
             /model <grok> returns to Grok."
        )));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_label_uses_display_name() {
        let session = CursorClientSession {
            model_id: "composer-2".into(),
            display_name: "Composer 2".into(),
            agent_id: "agt".into(),
            inflight: false,
            activity: None,
            stream_entry: None,
            streamed_text: String::new(),
        };
        assert_eq!(status_label(&session), "Cursor · Composer 2");
    }

    #[test]
    fn prefetch_triggers_on_model_cursor_prefix() {
        clear_cached_models_for_test();
        assert!(should_prefetch_models("/model-cursor"));
        assert!(should_prefetch_models("/model-cursor "));
        assert!(should_prefetch_models("/cursor-model composer"));
        assert!(!should_prefetch_models("/model grok"));
        assert!(!should_prefetch_models("model-cursor"));
        set_cached_models(vec![CursorModelChoice {
            id: "composer-2".into(),
            display_name: "Composer 2".into(),
            description: String::new(),
        }]);
        assert!(!should_prefetch_models("/model-cursor"));
        clear_cached_models_for_test();
    }
}
