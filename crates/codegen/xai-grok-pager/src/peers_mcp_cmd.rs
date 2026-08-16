//! `grok peers-mcp` — stdio MCP for Cursor client mode.

pub async fn run() -> std::io::Result<()> {
    xai_grok_peers::run_peers_mcp_stdio().await
}

pub async fn run_selftest() -> std::io::Result<()> {
    xai_grok_peers::selftest_peers_mcp()
        .await
        .map_err(std::io::Error::other)
}
