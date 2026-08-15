use crate::error::CursorSdkError;

/// Process env var for the Cursor user/service API key (never `XAI_API_KEY`).
pub const CURSOR_API_KEY_ENV: &str = "CURSOR_API_KEY";

/// Read `cursor::api_key` from `~/.grok/auth.json` without touching xAI scopes.
pub fn read_stored_cursor_api_key() -> Option<String> {
    let path = xai_grok_config::grok_home().join("auth.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("cursor::api_key")
        .and_then(|e| e.get("key"))
        .and_then(|k| k.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Resolve the Cursor API key: `CURSOR_API_KEY` first, then `read_disk`.
pub fn resolve_cursor_api_key(
    read_disk: impl FnOnce() -> Option<String>,
) -> Result<String, CursorSdkError> {
    if let Ok(key) = std::env::var(CURSOR_API_KEY_ENV) {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    match read_disk() {
        Some(key) if !key.trim().is_empty() => Ok(key.trim().to_string()),
        _ => Err(CursorSdkError::missing_api_key()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn env_wins_over_disk() {
        let _serial = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(CURSOR_API_KEY_ENV, "env-key");
        let got = resolve_cursor_api_key(|| Some("disk-key".into())).unwrap();
        assert_eq!(got, "env-key");
    }

    #[test]
    fn disk_used_when_env_absent() {
        let _serial = env_lock().lock().unwrap();
        let _guard = EnvGuard::remove(CURSOR_API_KEY_ENV);
        let got = resolve_cursor_api_key(|| Some("disk-key".into())).unwrap();
        assert_eq!(got, "disk-key");
    }

    #[test]
    fn missing_both_is_error() {
        let _serial = env_lock().lock().unwrap();
        let _guard = EnvGuard::remove(CURSOR_API_KEY_ENV);
        let err = resolve_cursor_api_key(|| None).unwrap_err();
        assert!(err.to_string().contains("login-cursor"));
    }

    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }

        fn remove(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
}
