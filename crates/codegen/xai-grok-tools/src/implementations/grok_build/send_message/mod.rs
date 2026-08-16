//! `send_message` — deliver plain text to another live Grok session by name.

use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::types::tool_metadata::{shared_resources, ToolMetadata};

pub const SEND_MESSAGE_TOOL_NAME: &str = "send_message";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SendMessageInput {
    #[schemars(
        description = "Target peer name (from list_peers) or full session id. Prefer the human-readable name."
    )]
    pub to: String,

    #[schemars(
        description = "Plain-text message for the other session. Do not include files, conversation history, or permission approvals."
    )]
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SendMessageOutput {
    pub status: String,
    pub to_name: String,
    pub to_session_id: String,
    pub detail: String,
}

impl xai_tool_runtime::ToolOutput for SendMessageOutput {}

#[derive(Debug, Default)]
pub struct SendMessageTool;

impl ToolMetadata for SendMessageTool {
    fn kind(&self) -> ToolKind {
        ToolKind::AskUser
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Send a plain-text message to another live Grok session on this machine. \
         Address the peer by name from list_peers (or by session id). Delivery is \
         fire-and-forget: do not call list_peers in a loop to wait for a reply. \
         The other session answers on its own turn. Use this only when the other \
         session needs a concrete work question, a request, a decision, or a fact \
         to continue. Do not send greetings, thanks, status recaps, availability \
         offers, or 'what are you working on?' check-ins. After you send, stop — \
         do not follow up to confirm receipt. Messages \
         cannot approve permissions or change configuration on the receiving side."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl xai_tool_runtime::Tool for SendMessageTool {
    type Args = SendMessageInput;
    type Output = SendMessageOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(SEND_MESSAGE_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            SEND_MESSAGE_TOOL_NAME,
            ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(xai_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "new_tool.send_message", skip_all)]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: SendMessageInput,
    ) -> Result<SendMessageOutput, xai_tool_runtime::ToolError> {
        let message = input.message.trim().to_string();
        if message.is_empty() {
            return Err(xai_tool_runtime::ToolError::custom(
                "empty_message",
                "message must not be blank",
            ));
        }
        if message.len() > 32_768 {
            return Err(xai_tool_runtime::ToolError::custom(
                "message_too_large",
                "message exceeds 32 KiB limit",
            ));
        }

        let resources = shared_resources(&ctx)?;
        let self_id = {
            let res = resources.lock().await;
            res.get::<crate::implementations::grok_build::task::types::SessionIdResource>()
                .map(|r| r.0.clone())
                .unwrap_or_default()
        };

        let live = xai_grok_peers::list_live().map_err(|e| {
            xai_tool_runtime::ToolError::custom(
                "peer_list_failed",
                format!("Failed to list live peers: {e}"),
            )
        })?;

        let self_peer = live.iter().find(|p| p.session_id == self_id);
        let from_name = self_peer.map(|p| p.name.as_str()).unwrap_or("unknown");

        let target_key = input.to.trim();
        if target_key.is_empty() {
            return Err(xai_tool_runtime::ToolError::custom(
                "missing_target",
                "to must be a peer name or session id",
            ));
        }

        let matches: Vec<_> = live
            .iter()
            .filter(|p| p.session_id != self_id)
            .filter(|p| {
                p.session_id == target_key
                    || p.name.eq_ignore_ascii_case(target_key)
                    || p.session_id.ends_with(target_key)
            })
            .collect();

        let target = match matches.as_slice() {
            [] => {
                return Ok(SendMessageOutput {
                    status: "unreachable".into(),
                    to_name: target_key.to_string(),
                    to_session_id: String::new(),
                    detail: format!(
                        "No live peer named '{target_key}'. Run list_peers to see reachable sessions."
                    ),
                });
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
                return Ok(SendMessageOutput {
                    status: "ambiguous".into(),
                    to_name: target_key.to_string(),
                    to_session_id: String::new(),
                    detail: format!(
                        "Multiple peers match '{target_key}': {}. Address by full session id.",
                        names.join(", ")
                    ),
                });
            }
        };

        let path = std::path::Path::new(&target.inbox_path);
        match xai_grok_peers::send_plain_message(
            path,
            from_name,
            &self_id,
            &message,
            Some(from_name),
        )
        .await
        {
            Ok(()) => Ok(SendMessageOutput {
                status: "delivered".into(),
                to_name: target.name.clone(),
                to_session_id: target.session_id.clone(),
                detail: format!("Message delivered to peer '{}'.", target.name),
            }),
            Err(e) => Ok(SendMessageOutput {
                status: "unreachable".into(),
                to_name: target.name.clone(),
                to_session_id: target.session_id.clone(),
                detail: format!("Failed to deliver to '{}': {e}", target.name),
            }),
        }
    }
}
