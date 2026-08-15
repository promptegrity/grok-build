//! `list_peers` — discover other live Grok sessions on this machine.

use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::types::tool_metadata::{ToolMetadata, shared_resources};

pub const LIST_PEERS_TOOL_NAME: &str = "list_peers";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ListPeersInput {
    /// Optional substring filter matched against peer name or cwd (case-insensitive).
    #[serde(default)]
    #[schemars(description = "Optional case-insensitive filter matched against peer name or cwd.")]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PeerInfo {
    pub name: String,
    pub session_id: String,
    pub cwd: String,
    pub short_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ListPeersOutput {
    pub peers: Vec<PeerInfo>,
    pub self_name: Option<String>,
    pub self_session_id: String,
}

impl xai_tool_runtime::ToolOutput for ListPeersOutput {}

#[derive(Debug, Default)]
pub struct ListPeersTool;

impl ToolMetadata for ListPeersTool {
    fn kind(&self) -> ToolKind {
        ToolKind::List
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "List other live Grok sessions on this machine that you can message with send_message. \
         Each peer has a name (set via --name or /rename), working directory, and session id. \
         A note such as cursor:<model> means that peer is a Cursor client and will forward \
         your message to Cursor, then reply on its own. \
         Use this before send_message when you need to discover which session to address. \
         Do not poll list_peers to wait for a reply."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl xai_tool_runtime::Tool for ListPeersTool {
    type Args = ListPeersInput;
    type Output = ListPeersOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(LIST_PEERS_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            LIST_PEERS_TOOL_NAME,
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

    #[tracing::instrument(name = "new_tool.list_peers", skip_all)]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: ListPeersInput,
    ) -> Result<ListPeersOutput, xai_tool_runtime::ToolError> {
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

        let self_name = live
            .iter()
            .find(|p| p.session_id == self_id)
            .map(|p| p.name.clone());

        let filter = input
            .filter
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty());

        let peers: Vec<PeerInfo> = live
            .into_iter()
            .filter(|p| p.session_id != self_id)
            .filter(|p| match &filter {
                None => true,
                Some(f) => {
                    p.name.to_ascii_lowercase().contains(f)
                        || p.cwd.to_ascii_lowercase().contains(f)
                }
            })
            .map(|p| PeerInfo {
                short_id: short_session_id(&p.session_id),
                name: p.name,
                session_id: p.session_id,
                cwd: p.cwd,
                note: p.note,
            })
            .collect();

        Ok(ListPeersOutput {
            peers,
            self_name,
            self_session_id: self_id,
        })
    }
}

fn short_session_id(session_id: &str) -> String {
    let compact: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if compact.len() <= 8 {
        compact
    } else {
        compact[compact.len() - 8..].to_string()
    }
}
