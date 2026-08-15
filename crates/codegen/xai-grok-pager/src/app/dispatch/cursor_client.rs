//! Dispatch for Cursor client/proxy mode (`/model-cursor`).

use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::app::app_view::{ActiveView, AppView};
use crate::cursor_client::{self, CursorModelChoice};
use crate::scrollback::block::RenderBlock;

pub(super) fn dispatch_fetch_cursor_models(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get(&id) else {
        return vec![];
    };
    if !cursor_client::mark_fetch_started() && cursor_client::cached_models().is_some() {
        return vec![];
    }
    vec![Effect::FetchCursorModels {
        agent_id: id,
        cwd: agent.session.cwd.clone(),
    }]
}

pub(super) fn dispatch_activate_cursor_client(
    app: &mut AppView,
    model_id: String,
    display_name: String,
) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    if agent.session.session_id.is_none() {
        agent
            .scrollback
            .push_block(RenderBlock::system("Start a session before /model-cursor."));
        return vec![];
    }
    vec![Effect::CreateCursorAgent {
        agent_id: id,
        cwd: agent.session.cwd.clone(),
        model_id,
        display_name,
    }]
}

pub(super) fn dispatch_cursor_proxy_inbound(
    app: &mut AppView,
    session_id: &str,
    text: String,
    reply_to: Option<String>,
    from_name: Option<String>,
) -> Vec<Effect> {
    let Some(agent) = super::ctx::find_agent_by_session_id(&mut app.agents, session_id) else {
        return vec![];
    };
    if agent.cursor_client.is_none() {
        return vec![];
    }
    let display = match from_name.as_deref().filter(|s| !s.is_empty()) {
        Some(from) => format!("[from {from}]\n{text}"),
        None => text.clone(),
    };
    cursor_client::enqueue_or_send(agent, text, reply_to, Some(display))
}

pub(super) fn handle_cursor_models_loaded(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<Vec<CursorModelChoice>, String>,
) -> Vec<Effect> {
    match result {
        Ok(models) => {
            cursor_client::set_cached_models(models);
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.scrollback.push_block(RenderBlock::system(
                    "Cursor models loaded. Pick one with /model-cursor <name>.",
                ));
            }
        }
        Err(error) => {
            cursor_client::reset_models_fetch();
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.scrollback.push_block(RenderBlock::system(format!(
                    "Failed to list Cursor models: {error}"
                )));
            }
        }
    }
    vec![]
}

pub(super) fn handle_cursor_agent_created(
    app: &mut AppView,
    agent_id: AgentId,
    model_id: String,
    display_name: String,
    result: Result<String, String>,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    match result {
        Ok(cursor_agent_id) => {
            cursor_client::activate_on_agent(agent, model_id, display_name, cursor_agent_id);
        }
        Err(error) => {
            agent.scrollback.push_block(RenderBlock::system(format!(
                "Failed to create Cursor agent: {error}"
            )));
        }
    }
    vec![]
}

pub(super) fn handle_cursor_proxy_send_complete(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<String, String>,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    match result {
        Ok(text) => {
            let body = if text.trim().is_empty() {
                "(Cursor finished with no text.)".to_string()
            } else {
                text
            };
            agent
                .scrollback
                .push_block(RenderBlock::agent_message(body));
        }
        Err(error) => {
            agent
                .scrollback
                .push_block(RenderBlock::system(format!("Cursor send failed: {error}")));
        }
    }
    cursor_client::drain_queued_send(agent)
}

/// Shared exit used by `/model` so a Grok model switch leaves client mode.
pub(super) fn leave_cursor_client_if_active(app: &mut AppView, agent_id: AgentId) {
    cursor_client::clear_cursor_client(app, agent_id);
}
