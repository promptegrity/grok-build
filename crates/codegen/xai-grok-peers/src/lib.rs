//! Live peer registry and local inbox messaging between Grok sessions.
//!
//! Each interactive (or headless) session that opts into cross-session messaging
//! writes a [`PeerRecord`] under `~/.grok/peers/` and binds an inbox Unix socket
//! (named pipe on Windows). Other sessions discover peers by listing the
//! registry and deliver plain-text messages by connecting to the inbox path.

mod inbox;
mod naming;
mod registry;

pub use inbox::{
    InboxEnvelope, InboxHandle, InboxListenError, InboxSendError, MAX_QUEUED_PEER_MESSAGES,
    RepeatGuard, bind_inbox, format_inbound_prompt, inbox_path_for_session, read_envelope_line,
    send_envelope, send_plain_message,
};
pub use naming::{allocate_live_name, default_name_from_cwd, sanitize_peer_name};
pub use registry::{
    PeerRecord, list_live, list_live_in, register, register_in, unregister, unregister_in,
    update_name, update_name_in, update_note, update_note_in,
};

/// Directory name under grok home that holds peer JSON files and sockets.
pub const PEERS_DIR: &str = "peers";

/// Resolve `~/.grok/peers` (or `$GROK_HOME/peers`).
pub fn peers_dir() -> std::path::PathBuf {
    xai_grok_config::grok_home().join(PEERS_DIR)
}

pub fn peers_dir_in(root: &std::path::Path) -> std::path::PathBuf {
    root.join(PEERS_DIR)
}
