//! `/peers` (alias `/list-agents`) — list live peer sessions for messaging.

use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// List other live Grok sessions reachable for cross-session messaging.
pub struct PeersCommand;

impl SlashCommand for PeersCommand {
    fn name(&self) -> &str {
        "peers"
    }

    fn aliases(&self) -> &[&str] {
        &["list-agents", "list_agents"]
    }

    fn description(&self) -> &str {
        "List live sessions you can message"
    }

    fn session_scoped(&self) -> bool {
        false
    }

    fn usage(&self) -> &str {
        "/peers"
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        let self_id = ctx.session_id.map(|id| id.0.as_ref());
        match xai_grok_peers::list_live() {
            Ok(peers) => {
                if peers.is_empty() {
                    return CommandResult::Message(
                        "No live peer sessions on this machine.\n\
                         Start another `grok --name <name>` in a second terminal to message it."
                            .into(),
                    );
                }
                let mut lines = Vec::new();
                lines.push("Live peer sessions:".to_string());
                for peer in &peers {
                    let is_self = self_id.is_some_and(|id| id == peer.session_id);
                    let marker = if is_self { " (this session)" } else { "" };
                    let short: String = peer
                        .session_id
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .rev()
                        .take(8)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    lines.push(format!(
                        "  • {name}{marker}\n    cwd: {cwd}\n    id: …{short}",
                        name = peer.name,
                        marker = marker,
                        cwd = peer.cwd,
                        short = short,
                    ));
                }
                lines.push(String::new());
                lines.push(
                    "Ask Grok to message a peer by name, e.g. \"tell api the migration finished\"."
                        .into(),
                );
                CommandResult::Message(lines.join("\n"))
            }
            Err(e) => CommandResult::Error(format!("Failed to list peers: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peers_command_metadata() {
        let cmd = PeersCommand;
        assert_eq!(cmd.name(), "peers");
        assert!(cmd.aliases().contains(&"list-agents"));
        assert_eq!(cmd.usage(), "/peers");
    }
}
