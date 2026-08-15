//! Peer name sanitization and collision handling.

use std::collections::HashSet;

/// Max length for a peer display name (aligned with session title limits).
pub const MAX_PEER_NAME_LEN: usize = 80;

/// Sanitize a user-supplied name: trim, collapse whitespace, strip controls.
pub fn sanitize_peer_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut out = String::with_capacity(trimmed.len().min(MAX_PEER_NAME_LEN));
    let mut prev_space = false;
    for ch in trimmed.chars() {
        if out.chars().count() >= MAX_PEER_NAME_LEN {
            break;
        }
        if ch.is_control() {
            continue;
        }
        if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        prev_space = false;
        out.push(ch);
    }
    out.trim().to_string()
}

/// Derive a default peer name from the working directory folder + short id.
///
/// Example: cwd `/home/u/my-app`, session `…3f91` → `my-app-3f`.
pub fn default_name_from_cwd(cwd: &str, session_id: &str) -> String {
    let folder = std::path::Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("session");
    let folder = sanitize_peer_name(folder);
    let folder = if folder.is_empty() {
        "session".to_string()
    } else {
        folder
    };
    let suffix = short_id_suffix(session_id);
    format!("{folder}-{suffix}")
}

fn short_id_suffix(session_id: &str) -> String {
    let compact: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let take = compact
        .len()
        .min(2)
        .max(if compact.is_empty() { 0 } else { 2 });
    if take == 0 {
        "00".to_string()
    } else {
        compact[compact.len().saturating_sub(take)..].to_lowercase()
    }
}

/// Pick a live name that does not collide with `taken` (case-insensitive).
///
/// If `preferred` is free, return it. Otherwise append `-2`, `-3`, …
pub fn allocate_live_name(preferred: &str, taken: &HashSet<String>) -> String {
    let base = {
        let s = sanitize_peer_name(preferred);
        if s.is_empty() {
            "session".to_string()
        } else {
            s
        }
    };
    let lower_taken: HashSet<String> = taken.iter().map(|s| s.to_ascii_lowercase()).collect();
    if !lower_taken.contains(&base.to_ascii_lowercase()) {
        return base;
    }
    for n in 2u32..10_000 {
        let candidate = format!("{base}-{n}");
        if !lower_taken.contains(&candidate.to_ascii_lowercase()) {
            return candidate;
        }
    }
    format!("{base}-{}", uuid_fallback_suffix())
}

fn uuid_fallback_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos % 0xffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_controls_and_collapses_space() {
        assert_eq!(sanitize_peer_name("  api\t worker\n "), "api worker");
    }

    #[test]
    fn default_name_uses_folder_and_suffix() {
        let name = default_name_from_cwd("/tmp/my-app", "abcdef12-3456");
        assert_eq!(name, "my-app-56");
    }

    #[test]
    fn allocate_suffixes_on_collision() {
        let mut taken = HashSet::new();
        taken.insert("api".into());
        taken.insert("api-2".into());
        assert_eq!(allocate_live_name("api", &taken), "api-3");
        assert_eq!(allocate_live_name("API", &taken), "API-3");
    }

    #[test]
    fn allocate_free_name_unchanged() {
        let taken = HashSet::new();
        assert_eq!(allocate_live_name("frontend", &taken), "frontend");
    }
}
