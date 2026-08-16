//! `grok peers-mcp` — stdio MCP for Cursor client mode.

pub async fn run() -> std::io::Result<()> {
    xai_grok_peers::run_peers_mcp_stdio().await
}
