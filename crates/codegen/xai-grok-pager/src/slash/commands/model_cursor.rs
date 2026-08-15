//! `/model-cursor` (alias `/cursor-model`) — enter Cursor client/proxy mode.

use crate::app::actions::Action;
use crate::cursor_client::{self, CursorModelChoice};
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

/// Switch this session into a Cursor client bound to a Cursor model.
pub struct ModelCursorCommand;

impl SlashCommand for ModelCursorCommand {
    fn name(&self) -> &str {
        "model-cursor"
    }

    fn aliases(&self) -> &[&str] {
        &["cursor-model"]
    }

    fn description(&self) -> &str {
        "Use a Cursor model as a client (proxy)"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn offered_when_session_less(&self) -> bool {
        false
    }

    fn usage(&self) -> &str {
        "/model-cursor <name>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<cursor-model>")
    }

    fn suggest_args(&self, _ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        let models = cursor_client::cached_models()?;
        if models.is_empty() {
            return None;
        }
        Some(models.iter().map(arg_item).collect())
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return CommandResult::Error("Usage: /model-cursor <name>".into());
        }
        let Some((model_id, display_name)) = cursor_client::resolve_model_arg(trimmed) else {
            return CommandResult::Error("Usage: /model-cursor <name>".into());
        };
        CommandResult::Action(Action::ActivateCursorClient {
            model_id,
            display_name,
        })
    }
}

fn arg_item(model: &CursorModelChoice) -> ArgItem {
    let display = if model.display_name.trim().is_empty() {
        model.id.clone()
    } else {
        model.display_name.clone()
    };
    ArgItem {
        display: display.clone(),
        match_text: format!("{} {}", model.id, display),
        insert_text: model.id.clone(),
        description: if model.description.is_empty() {
            model.id.clone()
        } else {
            model.description.clone()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::slash::command::CommandExecCtx;

    static DEFAULT_BUNDLE_STATE: crate::app::bundle::BundleState =
        crate::app::bundle::BundleState {
            has_cache: false,
            version: String::new(),
            personas: Vec::new(),
            roles: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        };

    fn make_ctx(models: &ModelState) -> CommandExecCtx<'_> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    fn make_app_ctx(models: &ModelState) -> AppCtx<'_> {
        AppCtx {
            models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: true,
            screen_mode: crate::app::ScreenMode::Fullscreen,
            current_title: None,
        }
    }

    #[test]
    fn empty_args_are_an_error() {
        cursor_client::clear_cached_models_for_test();
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        match ModelCursorCommand.run(&mut ctx, "") {
            CommandResult::Error(msg) => assert!(msg.contains("/model-cursor")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn raw_id_activates_without_cache() {
        cursor_client::clear_cached_models_for_test();
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        match ModelCursorCommand.run(&mut ctx, "composer-2") {
            CommandResult::Action(Action::ActivateCursorClient {
                model_id,
                display_name,
            }) => {
                assert_eq!(model_id, "composer-2");
                assert_eq!(display_name, "composer-2");
            }
            other => panic!("expected ActivateCursorClient, got {other:?}"),
        }
    }

    #[test]
    fn resolves_display_name_from_cache() {
        cursor_client::clear_cached_models_for_test();
        cursor_client::set_cached_models(vec![CursorModelChoice {
            id: "composer-2".into(),
            display_name: "Composer 2".into(),
            description: "test".into(),
        }]);
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        match ModelCursorCommand.run(&mut ctx, "Composer 2") {
            CommandResult::Action(Action::ActivateCursorClient {
                model_id,
                display_name,
            }) => {
                assert_eq!(model_id, "composer-2");
                assert_eq!(display_name, "Composer 2");
            }
            other => panic!("expected ActivateCursorClient, got {other:?}"),
        }
        cursor_client::clear_cached_models_for_test();
    }

    #[test]
    fn suggest_args_lists_cached_models() {
        cursor_client::clear_cached_models_for_test();
        cursor_client::set_cached_models(vec![CursorModelChoice {
            id: "composer-2".into(),
            display_name: "Composer 2".into(),
            description: "fast".into(),
        }]);
        let models = ModelState::default();
        let ctx = make_app_ctx(&models);
        let items = ModelCursorCommand.suggest_args(&ctx, "").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text, "composer-2");
        cursor_client::clear_cached_models_for_test();
    }

    #[test]
    fn suggest_args_none_without_cache() {
        cursor_client::clear_cached_models_for_test();
        let models = ModelState::default();
        let ctx = make_app_ctx(&models);
        assert!(ModelCursorCommand.suggest_args(&ctx, "").is_none());
    }
}
