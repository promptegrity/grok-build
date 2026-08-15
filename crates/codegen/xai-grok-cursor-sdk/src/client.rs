use std::path::PathBuf;
use std::sync::Arc;

use crate::auth::resolve_cursor_api_key;
use crate::bridge::{BridgeHandle, BridgeManager};
use crate::error::CursorSdkError;
use crate::pb::{
    AgentOptions, CreateAgentRequest, CursorRequestOptions, GetRunOptions, GetRunRequest,
    ListAgentsOptions, ListAgentsRequest, ListModelsRequest, LocalAgentOptions, ModelSelection,
    RunLifecycleStatus, RunStreamMessage, SendRequest, UserMessage, WaitLiveRunRequest,
    run_stream_message,
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
    pub fn connect(
        workspace: PathBuf,
        read_disk_key: impl FnOnce() -> Option<String>,
    ) -> Result<Self, CursorSdkError> {
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
        self.send_with_progress(agent_id, text, |_| {}).await
    }

    /// Send a prompt and report stream events as they arrive (status / steps / text).
    pub async fn send_with_progress(
        &self,
        agent_id: &str,
        text: &str,
        mut on_event: impl FnMut(CursorRunEvent),
    ) -> Result<SendResult, CursorSdkError> {
        let handle = self.handle().await?;
        let outcome = transport::server_stream_outcome_on(
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
            |msg| {
                if let Some(event) = run_event(msg) {
                    on_event(event);
                }
            },
        )
        .await?;

        let mut folded = fold_run_stream(&outcome.messages);

        // Stream dropped or finished without assistant text: block on WaitLiveRun
        // so the Grok tool call does not return and trigger a poll loop.
        if folded.assistant.is_empty() && !folded.run_id.is_empty() {
            on_event(CursorRunEvent {
                kind: CursorRunEventKind::Status,
                text: "waiting for Cursor to finish".into(),
            });
            match self.wait_live_run_until_done(&folded.run_id).await {
                Ok(text) if !text.is_empty() => {
                    on_event(CursorRunEvent {
                        kind: CursorRunEventKind::Assistant,
                        text: text.clone(),
                    });
                    folded.assistant = text;
                }
                Ok(_) => {}
                Err(e) => {
                    if folded.assistant.is_empty() && outcome.error.is_some() {
                        return Err(e);
                    }
                }
            }
        }

        if folded.assistant.is_empty()
            && folded.run_id.is_empty()
            && let Some(e) = outcome.error
        {
            return Err(e);
        }

        Ok(SendResult {
            run_id: folded.run_id,
            text: folded.assistant,
            status: folded.status,
            error: folded.error,
        })
    }

    pub async fn wait_live_run(&self, run_id: &str) -> Result<String, CursorSdkError> {
        let handle = self.handle().await?;
        let resp: crate::pb::WaitLiveRunResponse = transport::unary_with_timeout(
            handle.http(),
            &handle.url,
            AGENT_SERVICE,
            "WaitLiveRun",
            &handle.bearer,
            &WaitLiveRunRequest {
                run_id: run_id.to_string(),
            },
            std::time::Duration::from_secs(60 * 60),
        )
        .await?;
        Ok(resp.result.map(|r| r.result).unwrap_or_default())
    }

    /// Block until the Cursor run is finished (or we have final text).
    async fn wait_live_run_until_done(&self, run_id: &str) -> Result<String, CursorSdkError> {
        if let Ok(text) = self.wait_live_run(run_id).await
            && !text.is_empty()
        {
            return Ok(text);
        }
        for _ in 0..8 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if let Ok(snap) = self.get_run(run_id).await {
                if !snap.result.is_empty() {
                    return Ok(snap.result);
                }
                if matches!(
                    snap.status(),
                    RunLifecycleStatus::Finished
                        | RunLifecycleStatus::Error
                        | RunLifecycleStatus::Cancelled
                        | RunLifecycleStatus::Expired
                ) {
                    return Ok(snap.result);
                }
            }
            if let Ok(text) = self.wait_live_run(run_id).await
                && !text.is_empty()
            {
                return Ok(text);
            }
        }
        self.wait_live_run(run_id).await
    }

    pub async fn get_run(&self, run_id: &str) -> Result<crate::pb::RunSnapshot, CursorSdkError> {
        let handle = self.handle().await?;
        let resp: crate::pb::GetRunResponse = transport::unary(
            handle.http(),
            &handle.url,
            AGENT_SERVICE,
            "GetRun",
            &handle.bearer,
            &GetRunRequest {
                run_id: run_id.to_string(),
                options: Some(GetRunOptions {
                    api_key: self.api_key.clone(),
                    cwd: self.workspace.to_str().unwrap_or_default().to_string(),
                    ..Default::default()
                }),
            },
        )
        .await?;
        resp.run.ok_or_else(|| {
            CursorSdkError::message(format!("GetRun returned no snapshot for {run_id}"))
        })
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
                    cwd: self.workspace.to_str().unwrap_or_default().to_string(),
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

/// Incremental event from a Cursor `Send` stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorRunEvent {
    pub kind: CursorRunEventKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorRunEventKind {
    Status,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct AgentSummary {
    pub agent_id: String,
    pub name: String,
    pub summary: String,
    pub archived: bool,
}

struct FoldedRun {
    assistant: String,
    status: String,
    run_id: String,
    error: Option<String>,
}

fn run_event(msg: &RunStreamMessage) -> Option<CursorRunEvent> {
    match &msg.envelope {
        Some(run_stream_message::Envelope::SdkMessage(m)) => {
            if (m.r#type == "assistant" || m.r#type == "assistant_message")
                && let Some(text) = struct_text(&m.message).filter(|s| !s.is_empty())
            {
                return Some(CursorRunEvent {
                    kind: CursorRunEventKind::Assistant,
                    text,
                });
            }
            if m.r#type == "status"
                && let Some(text) = struct_text(&m.message).filter(|s| !s.is_empty())
            {
                return Some(CursorRunEvent {
                    kind: CursorRunEventKind::Status,
                    text,
                });
            }
            None
        }
        Some(run_stream_message::Envelope::InteractionUpdate(u)) => {
            let text = struct_text(&u.update).filter(|s| !s.is_empty())?;
            if u.r#type.contains("assistant") || u.r#type.contains("text") {
                Some(CursorRunEvent {
                    kind: CursorRunEventKind::Assistant,
                    text,
                })
            } else {
                Some(CursorRunEvent {
                    kind: CursorRunEventKind::Status,
                    text,
                })
            }
        }
        Some(run_stream_message::Envelope::Step(s)) => {
            let text = step_label(&s.step)?;
            Some(CursorRunEvent {
                kind: CursorRunEventKind::Status,
                text,
            })
        }
        _ => None,
    }
}

fn step_label(step: &Option<pbjson_types::Struct>) -> Option<String> {
    for key in [
        "title",
        "name",
        "description",
        "type",
        "kind",
        "status",
        "tool",
    ] {
        if let Some(v) = struct_field_string(step, key).filter(|s| !s.is_empty()) {
            return Some(v);
        }
    }
    struct_text(step).filter(|s| !s.is_empty())
}

fn fold_run_stream(messages: &[RunStreamMessage]) -> FoldedRun {
    let mut assistant = String::new();
    let mut status = String::new();
    let mut run_id = String::new();
    let mut error = None;
    for msg in messages {
        match &msg.envelope {
            Some(run_stream_message::Envelope::SdkMessage(m)) => {
                if (m.r#type == "assistant" || m.r#type == "assistant_message")
                    && let Some(text) = struct_text(&m.message)
                {
                    if !assistant.is_empty() {
                        assistant.push('\n');
                    }
                    assistant.push_str(&text);
                } else if m.r#type == "status"
                    && let Some(text) = struct_text(&m.message)
                {
                    status = text;
                }
            }
            Some(run_stream_message::Envelope::Result(r)) => {
                if !r.run_id.is_empty() {
                    run_id = r.run_id.clone();
                }
                if let Some(code) = r.error_code.as_deref().filter(|s| !s.is_empty()) {
                    error = Some(code.to_string());
                }
                if let Some(result) = &r.result
                    && !result.result.is_empty()
                {
                    assistant = result.result.clone();
                }
            }
            Some(run_stream_message::Envelope::Done(d)) => {
                if run_id.is_empty() && !d.run_id.is_empty() {
                    run_id = d.run_id.clone();
                }
            }
            Some(run_stream_message::Envelope::InteractionUpdate(u)) => {
                if let Some(text) = struct_text(&u.update)
                    && (u.r#type.contains("assistant") || u.r#type.contains("text"))
                {
                    assistant.push_str(&text);
                }
            }
            Some(run_stream_message::Envelope::Step(s)) => {
                if run_id.is_empty()
                    && let Some(id) = struct_field_string(&s.step, "run_id")
                        .or_else(|| struct_field_string(&s.step, "runId"))
                {
                    run_id = id;
                }
            }
            None => {}
        }
    }
    FoldedRun {
        assistant,
        status,
        run_id,
        error,
    }
}

fn struct_field_string(value: &Option<pbjson_types::Struct>, key: &str) -> Option<String> {
    let s = value.as_ref()?;
    s.fields.get(key).and_then(proto_value_string)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::{RunResult, RunStreamDone, RunStreamResult, SdkMessage};

    #[test]
    fn fold_keeps_run_id_from_done_without_assistant() {
        let messages = vec![RunStreamMessage {
            envelope: Some(run_stream_message::Envelope::Done(RunStreamDone {
                agent_id: "a".into(),
                run_id: "run-1".into(),
            })),
            offset: None,
        }];
        let folded = fold_run_stream(&messages);
        assert_eq!(folded.run_id, "run-1");
        assert!(folded.assistant.is_empty());
    }

    #[test]
    fn fold_prefers_result_text() {
        let messages = vec![RunStreamMessage {
            envelope: Some(run_stream_message::Envelope::Result(RunStreamResult {
                agent_id: "a".into(),
                run_id: "run-2".into(),
                status: 3,
                error_code: None,
                result: Some(RunResult {
                    result: "final answer".into(),
                    ..Default::default()
                }),
            })),
            offset: None,
        }];
        let folded = fold_run_stream(&messages);
        assert_eq!(folded.run_id, "run-2");
        assert_eq!(folded.assistant, "final answer");
    }

    #[test]
    fn fold_reads_assistant_sdk_message() {
        let mut fields = std::collections::HashMap::new();
        fields.insert(
            "text".into(),
            pbjson_types::Value {
                kind: Some(pbjson_types::value::Kind::StringValue("hi".into())),
            },
        );
        let messages = vec![RunStreamMessage {
            envelope: Some(run_stream_message::Envelope::SdkMessage(SdkMessage {
                r#type: "assistant".into(),
                message: Some(pbjson_types::Struct { fields }),
            })),
            offset: None,
        }];
        let folded = fold_run_stream(&messages);
        assert_eq!(folded.assistant, "hi");
        let event = run_event(&messages[0]).expect("assistant event");
        assert_eq!(event.kind, CursorRunEventKind::Assistant);
        assert_eq!(event.text, "hi");
    }
}
