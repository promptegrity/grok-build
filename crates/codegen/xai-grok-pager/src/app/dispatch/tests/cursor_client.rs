use super::*;
use crate::cursor_client::CursorClientSession;

fn enter_cursor_client(app: &mut AppView) {
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.cursor_client = Some(CursorClientSession {
        model_id: "composer-2".into(),
        display_name: "Composer 2".into(),
        agent_id: "agt_test".into(),
        inflight: false,
    });
}

#[test]
fn send_prompt_in_cursor_mode_emits_proxy_send() {
    let mut app = test_app_with_agent();
    enter_cursor_client(&mut app);
    let effects = dispatch(Action::SendPrompt("fix the bug".into()), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CursorProxySend {
                cursor_agent_id,
                text,
                reply_to: None,
                ..
            }] if cursor_agent_id == "agt_test" && text == "fix the bug"
        ),
        "expected CursorProxySend, got {effects:?}"
    );
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::SendPrompt { .. }))
    );
}

#[test]
fn slash_help_stays_local_in_cursor_mode() {
    let mut app = test_app_with_agent();
    enter_cursor_client(&mut app);
    let effects = dispatch(Action::SendPrompt("/help".into()), &mut app);
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::CursorProxySend { .. })),
        "slash commands must not go to Cursor, got {effects:?}"
    );
    assert!(app.agents[&AgentId(0)].cursor_client.is_some());
}

#[test]
fn slash_model_leaves_cursor_mode() {
    let mut app = test_app_with_agent();
    enter_cursor_client(&mut app);
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("grok-4.5"));
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .models
        .available
        .insert(
            model_id.clone(),
            acp::ModelInfo::new(model_id.clone(), "Grok 4.5".to_string()),
        );
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .models
        .set_current(model_id, None);
    let effects = dispatch(Action::SendPrompt("/model Grok 4.5".into()), &mut app);
    assert!(app.agents[&id].cursor_client.is_none());
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::CursorProxySend { .. })),
        "leaving Cursor must not proxy, got {effects:?}"
    );
}

#[test]
fn activate_cursor_client_emits_create_agent() {
    let mut app = test_app_with_agent();
    let effects = dispatch(
        Action::ActivateCursorClient {
            model_id: "composer-2".into(),
            display_name: "Composer 2".into(),
        },
        &mut app,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CreateCursorAgent {
                model_id,
                display_name,
                ..
            }] if model_id == "composer-2" && display_name == "Composer 2"
        ),
        "expected CreateCursorAgent, got {effects:?}"
    );
}

#[test]
fn peer_inbound_forwards_when_client_active() {
    let mut app = test_app_with_agent();
    enter_cursor_client(&mut app);
    let effects = dispatch(
        Action::CursorProxyInbound {
            session_id: "test-session".into(),
            text: "from t1".into(),
            reply_to: Some("api".into()),
            from_name: Some("api".into()),
        },
        &mut app,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CursorProxySend {
                text,
                reply_to: Some(reply),
                ..
            }] if text == "from t1" && reply == "api"
        ),
        "expected proxy send with reply_to, got {effects:?}"
    );
}

#[test]
fn cursor_models_loaded_error_surfaces_login_hint() {
    let mut app = test_app_with_agent();
    crate::cursor_client::clear_cached_models_for_test();
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CursorModelsLoaded {
            agent_id: AgentId(0),
            result: Err(
                "Cursor API key not set. Run `grok login-cursor` or export CURSOR_API_KEY".into(),
            ),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert!(crate::cursor_client::cached_models().is_none());
    let texts: Vec<&str> = app.agents[&AgentId(0)]
        .scrollback
        .iter_entries()
        .filter_map(|(_, e)| match &e.block {
            crate::scrollback::block::RenderBlock::System(s) => Some(s.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("login-cursor")),
        "missing-key fetch must tell the user to run grok login-cursor, got {texts:?}"
    );
}

#[test]
fn peer_inbound_ignored_when_not_client() {
    let mut app = test_app_with_agent();
    let effects = dispatch(
        Action::CursorProxyInbound {
            session_id: "test-session".into(),
            text: "from t1".into(),
            reply_to: Some("api".into()),
            from_name: Some("api".into()),
        },
        &mut app,
    );
    assert!(effects.is_empty());
}
