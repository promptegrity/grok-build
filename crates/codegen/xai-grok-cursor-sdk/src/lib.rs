//! Thin Cursor SDK Bridge adapter for Grok.
//!
//! Spawns the `cursor-sdk-bridge` sidecar, speaks Connect/`sdk.v1` over
//! HTTP/1.1, and exposes a small client used by Grok tools.

#![allow(clippy::derive_partial_eq_without_eq)]

pub mod auth;
pub mod bridge;
pub mod client;
pub mod discover;
pub mod error;
pub mod handshake;
pub mod transport;

/// Generated `sdk.v1` protobuf types (tonic stubs are unused).
pub mod pb {
    #![allow(clippy::large_enum_variant)]
    include!(concat!(env!("OUT_DIR"), "/sdk.v1.rs"));
}

pub use auth::{CURSOR_API_KEY_ENV, read_stored_cursor_api_key, resolve_cursor_api_key};
pub use bridge::{BridgeHandle, BridgeManager};
pub use client::CursorSdkClient;
pub use discover::{BRIDGE_BIN_ENV, discover_bridge_bin};
pub use error::CursorSdkError;
pub use handshake::{READY_LINE_PREFIX, ReadyInfo, parse_ready_line};
