//! Cursor SDK Bridge tools (`cursor_list_models`, `cursor_create_agent`, …).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::Cwd;
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::types::tool_metadata::{ToolMetadata, shared_resources};
use xai_grok_cursor_sdk::{CursorSdkClient, read_stored_cursor_api_key};

fn process_client(workspace: PathBuf) -> Result<Arc<CursorSdkClient>, xai_tool_runtime::ToolError> {
    static CLIENT: OnceLock<Arc<CursorSdkClient>> = OnceLock::new();
    if let Some(existing) = CLIENT.get() {
        return Ok(existing.clone());
    }
    let created = CursorSdkClient::connect(workspace, read_stored_cursor_api_key).map_err(|e| {
        xai_tool_runtime::ToolError::custom("cursor_sdk", e.to_string())
    })?;
    Ok(CLIENT.get_or_init(|| Arc::new(created)).clone())
}

async fn workspace_from_ctx(
    ctx: &xai_tool_runtime::ToolCallContext,
) -> Result<PathBuf, xai_tool_runtime::ToolError> {
    let resources = shared_resources(ctx)?;
    let res = resources.lock().await;
    Ok(res
        .get::<Cwd>()
        .map(|c| c.0.clone())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))))
}

pub const CURSOR_LIST_MODELS_TOOL_NAME: &str = "cursor_list_models";
pub const CURSOR_CREATE_AGENT_TOOL_NAME: &str = "cursor_create_agent";
pub const CURSOR_SEND_TOOL_NAME: &str = "cursor_send";
pub const CURSOR_LIST_AGENTS_TOOL_NAME: &str = "cursor_list_agents";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorListModelsInput {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorListModelsOutput {
    pub models: Vec<CursorModelInfo>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorModelInfo {
    pub id: String,
    pub display_name: String,
    pub description: String,
}

impl xai_tool_runtime::ToolOutput for CursorListModelsOutput {}

#[derive(Debug, Default)]
pub struct CursorListModelsTool;

impl ToolMetadata for CursorListModelsTool {
    fn kind(&self) -> ToolKind {
        ToolKind::List
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::Cursor
    }
    fn description_template(&self) -> &str {
        "List Cursor models available to the stored CURSOR_API_KEY via the SDK Bridge. \
         Use a returned id as the model argument to cursor_create_agent. \
         Requires `grok login-cursor` or CURSOR_API_KEY. This is not Grok /model."
    }
    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl xai_tool_runtime::Tool for CursorListModelsTool {
    type Args = CursorListModelsInput;
    type Output = CursorListModelsOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(CURSOR_LIST_MODELS_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            CURSOR_LIST_MODELS_TOOL_NAME,
            ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(xai_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        _input: CursorListModelsInput,
    ) -> Result<CursorListModelsOutput, xai_tool_runtime::ToolError> {
        let ws = workspace_from_ctx(&ctx).await?;
        let client = process_client(ws)?;
        let models = client.list_models().await.map_err(|e| {
            xai_tool_runtime::ToolError::custom("cursor_list_models", e.to_string())
        })?;
        Ok(CursorListModelsOutput {
            models: models
                .into_iter()
                .map(|m| CursorModelInfo {
                    id: m.id,
                    display_name: m.display_name,
                    description: m.description,
                })
                .collect(),
        })
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorCreateAgentInput {
    #[schemars(description = "Cursor model id from cursor_list_models (for example composer-2).")]
    pub model: String,
    #[serde(default)]
    #[schemars(description = "Optional display name for the Cursor agent.")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorCreateAgentOutput {
    pub agent_id: String,
    pub model: String,
}

impl xai_tool_runtime::ToolOutput for CursorCreateAgentOutput {}

#[derive(Debug, Default)]
pub struct CursorCreateAgentTool;

impl ToolMetadata for CursorCreateAgentTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::Cursor
    }
    fn description_template(&self) -> &str {
        "Create a local Cursor agent in the current workspace via the SDK Bridge. \
         Pass a model id from cursor_list_models. Returns agent_id for cursor_send."
    }
    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl xai_tool_runtime::Tool for CursorCreateAgentTool {
    type Args = CursorCreateAgentInput;
    type Output = CursorCreateAgentOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(CURSOR_CREATE_AGENT_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            CURSOR_CREATE_AGENT_TOOL_NAME,
            ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(xai_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: CursorCreateAgentInput,
    ) -> Result<CursorCreateAgentOutput, xai_tool_runtime::ToolError> {
        let model = input.model.trim();
        if model.is_empty() {
            return Err(xai_tool_runtime::ToolError::custom(
                "missing_model",
                "model is required (from cursor_list_models)",
            ));
        }
        let ws = workspace_from_ctx(&ctx).await?;
        let client = process_client(ws)?;
        let created = client
            .create_local_agent(model, input.name)
            .await
            .map_err(|e| xai_tool_runtime::ToolError::custom("cursor_create_agent", e.to_string()))?;
        Ok(CursorCreateAgentOutput {
            agent_id: created.agent_id,
            model: created.model,
        })
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorSendInput {
    #[schemars(description = "agent_id returned by cursor_create_agent or cursor_list_agents.")]
    pub agent_id: String,
    #[schemars(description = "User prompt to send to the Cursor agent.")]
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorSendOutput {
    pub run_id: String,
    pub text: String,
    pub status: String,
    pub error: Option<String>,
}

impl xai_tool_runtime::ToolOutput for CursorSendOutput {}

#[derive(Debug, Default)]
pub struct CursorSendTool;

impl ToolMetadata for CursorSendTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::Cursor
    }
    fn description_template(&self) -> &str {
        "Send a prompt to an existing Cursor agent and wait for the run result. \
         Use the same agent_id for multi-turn. Requires the SDK Bridge sidecar."
    }
    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl xai_tool_runtime::Tool for CursorSendTool {
    type Args = CursorSendInput;
    type Output = CursorSendOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(CURSOR_SEND_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            CURSOR_SEND_TOOL_NAME,
            ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(xai_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: CursorSendInput,
    ) -> Result<CursorSendOutput, xai_tool_runtime::ToolError> {
        if input.agent_id.trim().is_empty() {
            return Err(xai_tool_runtime::ToolError::custom(
                "missing_agent_id",
                "agent_id is required",
            ));
        }
        if input.message.trim().is_empty() {
            return Err(xai_tool_runtime::ToolError::custom(
                "empty_message",
                "message must not be blank",
            ));
        }
        let ws = workspace_from_ctx(&ctx).await?;
        let client = process_client(ws)?;
        let result = client
            .send(input.agent_id.trim(), input.message.trim())
            .await
            .map_err(|e| xai_tool_runtime::ToolError::custom("cursor_send", e.to_string()))?;
        Ok(CursorSendOutput {
            run_id: result.run_id,
            text: result.text,
            status: result.status,
            error: result.error,
        })
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorListAgentsInput {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorListAgentsOutput {
    pub agents: Vec<CursorAgentInfo>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CursorAgentInfo {
    pub agent_id: String,
    pub name: String,
    pub summary: String,
    pub archived: bool,
}

impl xai_tool_runtime::ToolOutput for CursorListAgentsOutput {}

#[derive(Debug, Default)]
pub struct CursorListAgentsTool;

impl ToolMetadata for CursorListAgentsTool {
    fn kind(&self) -> ToolKind {
        ToolKind::List
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::Cursor
    }
    fn description_template(&self) -> &str {
        "List Cursor agents visible to this workspace via the SDK Bridge. \
         Use an agent_id with cursor_send to continue a conversation."
    }
    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl xai_tool_runtime::Tool for CursorListAgentsTool {
    type Args = CursorListAgentsInput;
    type Output = CursorListAgentsOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(CURSOR_LIST_AGENTS_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            CURSOR_LIST_AGENTS_TOOL_NAME,
            ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(xai_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        _input: CursorListAgentsInput,
    ) -> Result<CursorListAgentsOutput, xai_tool_runtime::ToolError> {
        let ws = workspace_from_ctx(&ctx).await?;
        let client = process_client(ws)?;
        let agents = client.list_agents().await.map_err(|e| {
            xai_tool_runtime::ToolError::custom("cursor_list_agents", e.to_string())
        })?;
        Ok(CursorListAgentsOutput {
            agents: agents
                .into_iter()
                .map(|a| CursorAgentInfo {
                    agent_id: a.agent_id,
                    name: a.name,
                    summary: a.summary,
                    archived: a.archived,
                })
                .collect(),
        })
    }
}
