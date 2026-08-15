use std::path::PathBuf;
use std::sync::Arc;

use crate::auth::resolve_cursor_api_key;
use crate::bridge::{BridgeHandle, BridgeManager};
use crate::error::CursorSdkError;
use crate::pb::{
    AgentOptions, CreateAgentRequest, CursorRequestOptions, ListAgentsRequest, ListAgentsOptions,
    ListModelsRequest, LocalAgentOptions, ModelSelection, RunStreamMessage, SendRequest,
    UserMessage, WaitLiveRunRequest, run_stream_message,
};
use crate::transport;

const AGENT_SERVICE: &str = "sdk.v1.SdkAgentService";
const CURSOR_SERVICE: &str = "sdk.v1.SdkCursorService";

/// High-level Cursor SDK client used by Grok tools.
pub struct CursorSdkClient {
    manager: BridgeManager,
    api_key: String,
    workspace: PathBuf,
}

impl CursorSdkClient {
    pub fn connect(workspace: PathBuf, read_disk_key: impl FnOnce() -> Option<String>) -> Result<Self, CursorSdkError> {
        let api_key = resolve_cursor_api_key(read_disk_key)?;
        Ok(Self {
            manager: BridgeManager::new(workspace.clone(), api_key.clone()),
            api_key,
            workspace,
        })
    }

    pub async fn close(&self) {
        self.manager.close().await;
    }

    async fn handle(&self) -> Result<Arc<BridgeHandle>, CursorSdkError> {
        self.manager.handle().await
    }

    pub async fn list_models(&self) -> Result<Vec<crate::pb::SdkModel>, CursorSdkError> {
        let handle = self.handle().await?;
        let resp: crate::pb::ListModelsResponse = transport::unary(
            handle.http(),
            &handle.url,
            CURSOR_SERVICE,
            "ListModels",
            &handle.bearer,
            &ListModelsRequest {
                options: Some(CursorRequestOptions {
                    api_key: self.api_key.clone(),
                }),
            },
        )
        .await?;
        Ok(resp.items)
    }

    pub async fn create_local_agent(
        &self,
        model: &str,
        name: Option<String>,
    ) -> Result<CreateAgentResult, CursorSdkError> {
        let handle = self.handle().await?;
        let cwd = self
            .workspace
            .to_str()
            .ok_or_else(|| CursorSdkError::message("workspace path is not UTF-8"))?
            .to_string();
        let resp: crate::pb::CreateAgentResponse = transport::unary(
            handle.http(),
            &handle.url,
            AGENT_SERVICE,
            "CreateAgent",
            &handle.bearer,
            &CreateAgentRequest {
                options: Some(AgentOptions {
                    model: Some(ModelSelection {
                        id: model.to_string(),
                        params: Vec::new(),
                    }),
                    api_key: self.api_key.clone(),
                    name: name.unwrap_or_default(),
                    local: Some(LocalAgentOptions {
                        cwd: vec![cwd],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                idempotency_key: None,
            },
        )
        .await?;
        Ok(CreateAgentResult {
            agent_id: resp.agent_id,
            model: resp.model.map(|m| m.id).unwrap_or_default(),
        })
    }

    pub async fn send(&self, agent_id: &str, text: &str) -> Result<SendResult, CursorSdkError> {
        let handle = self.handle().await?;
        let messages: Vec<RunStreamMessage> = transport::server_stream(
            handle.http(),
            &handle.url,
            AGENT_SERVICE,
            "Send",
            &handle.bearer,
            &SendRequest {
                agent_id: agent_id.to_string(),
                message: Some(UserMessage {
                    text: text.to_string(),
                    images: Vec::new(),
                }),
                options: None,
                idempotency_key: None,
            },
        )
        .await?;

        let mut assistant = String::new();
        let mut status = String::new();
        let mut run_id = String::new();
        let mut error = None;
        for msg in messages {
            match msg.envelope {
                Some(run_stream_message::Envelope::SdkMessage(m)) => {
                    if m.r#type == "assistant" || m.r#type == "assistant_message" {
                        if let Some(text) = struct_text(&m.message) {
                            if !assistant.is_empty() {
                                assistant.push('\n');
                            }
                            assistant.push_str(&text);
                        }
                    } else if m.r#type == "status"
                        && let Some(text) = struct_text(&m.message)
                    {
                        status = text;
                    }
                }
                Some(run_stream_message::Envelope::Result(r)) => {
                    run_id = r.run_id;
                    if let Some(code) = r.error_code.filter(|s| !s.is_empty()) {
                        error = Some(code);
                    }
                    if let Some(result) = r.result
                        && !result.result.is_empty()
                    {
                        assistant = result.result;
                    }
                }
                Some(run_stream_message::Envelope::Done(d)) => {
                    if run_id.is_empty() {
                        run_id = d.run_id;
                    }
                }
                _ => {}
            }
        }

        if assistant.is_empty() && run_id.is_empty() {
            // Stream ended without a terminal result — try WaitLiveRun is not
            // possible without a run id; surface status if we have one.
        }

        if assistant.is_empty() && !run_id.is_empty() {
            if let Ok(wait) = self.wait_live_run(&run_id).await {
                assistant = wait;
            }
        }

        Ok(SendResult {
            run_id,
            text: assistant,
            status,
            error,
        })
    }

    pub async fn wait_live_run(&self, run_id: &str) -> Result<String, CursorSdkError> {
        let handle = self.handle().await?;
        let resp: crate::pb::WaitLiveRunResponse = transport::unary(
            handle.http(),
            &handle.url,
            AGENT_SERVICE,
            "WaitLiveRun",
            &handle.bearer,
            &WaitLiveRunRequest {
                run_id: run_id.to_string(),
            },
        )
        .await?;
        Ok(resp.result.map(|r| r.result).unwrap_or_default())
    }

    pub async fn list_agents(&self) -> Result<Vec<AgentSummary>, CursorSdkError> {
        let handle = self.handle().await?;
        let resp: crate::pb::ListAgentsResponse = transport::unary(
            handle.http(),
            &handle.url,
            AGENT_SERVICE,
            "ListAgents",
            &handle.bearer,
            &ListAgentsRequest {
                options: Some(ListAgentsOptions {
                    api_key: self.api_key.clone(),
                    cwd: self
                        .workspace
                        .to_str()
                        .unwrap_or_default()
                        .to_string(),
                    ..Default::default()
                }),
            },
        )
        .await?;
        Ok(resp
            .items
            .into_iter()
            .map(|a| AgentSummary {
                agent_id: a.agent_id,
                name: a.name,
                summary: a.summary,
                archived: a.archived,
            })
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct CreateAgentResult {
    pub agent_id: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct SendResult {
    pub run_id: String,
    pub text: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentSummary {
    pub agent_id: String,
    pub name: String,
    pub summary: String,
    pub archived: bool,
}

fn struct_text(value: &Option<pbjson_types::Struct>) -> Option<String> {
    let s = value.as_ref()?;
    if let Some(v) = s.fields.get("text").or_else(|| s.fields.get("message")) {
        return proto_value_string(v);
    }
    None
}

fn proto_value_string(v: &pbjson_types::Value) -> Option<String> {
    match &v.kind {
        Some(pbjson_types::value::Kind::StringValue(s)) => Some(s.clone()),
        Some(pbjson_types::value::Kind::NumberValue(n)) => Some(n.to_string()),
        Some(pbjson_types::value::Kind::BoolValue(b)) => Some(b.to_string()),
        _ => None,
    }
}
