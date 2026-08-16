use std::path::{Path, PathBuf};

use crate::error::CursorSdkError;

/// Override path to the sidecar binary.
pub const BRIDGE_BIN_ENV: &str = "CURSOR_SDK_BRIDGE_BIN";

/// Resolve the `grok` binary Cursor should spawn for `peers-mcp`.
///
/// `current_exe()` when it is a regular file, otherwise `which("grok")`,
/// otherwise the bare name `"grok"` (PATH lookup at spawn time).
pub fn resolve_grok_bin() -> String {
    resolve_grok_bin_with(std::env::current_exe().ok(), |name| which::which(name).ok())
}

pub(crate) fn resolve_grok_bin_with(
    current_exe: Option<PathBuf>,
    lookup_path: impl Fn(&str) -> Option<PathBuf>,
) -> String {
    if let Some(exe) = current_exe {
        let resolved = dunce_canonicalize(&exe).unwrap_or(exe);
        if resolved.is_file() {
            return resolved.to_string_lossy().into_owned();
        }
    }
    let grok_name = if cfg!(windows) { "grok.exe" } else { "grok" };
    if let Some(path) = lookup_path(grok_name) {
        return path.to_string_lossy().into_owned();
    }
    grok_name.to_string()
}

/// Locate `cursor-sdk-bridge`: env → sibling of `current_exe` → `~/.grok/bin` → PATH.
pub fn discover_bridge_bin() -> Result<PathBuf, CursorSdkError> {
    discover_bridge_bin_with(
        std::env::var_os(BRIDGE_BIN_ENV).map(PathBuf::from),
        std::env::current_exe().ok(),
        xai_grok_config::grok_home(),
        |name| which::which(name).ok(),
    )
}

pub(crate) fn discover_bridge_bin_with(
    env_override: Option<PathBuf>,
    current_exe: Option<PathBuf>,
    grok_home: PathBuf,
    lookup_path: impl Fn(&str) -> Option<PathBuf>,
) -> Result<PathBuf, CursorSdkError> {
    let exe_name = if cfg!(windows) {
        "cursor-sdk-bridge.exe"
    } else {
        "cursor-sdk-bridge"
    };

    if let Some(path) = env_override {
        if path.is_file() {
            return Ok(path);
        }
        return Err(CursorSdkError::BridgeNotFound(format!(
            "{BRIDGE_BIN_ENV}={path} is not a file",
            path = path.display()
        )));
    }

    if let Some(exe) = current_exe {
        let resolved = dunce_canonicalize(&exe).unwrap_or(exe);
        if let Some(dir) = resolved.parent() {
            let sibling = dir.join(exe_name);
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }

    let home_bin = grok_home.join("bin").join(exe_name);
    if home_bin.is_file() {
        return Ok(home_bin);
    }

    if let Some(path) = lookup_path(exe_name) {
        return Ok(path);
    }

    Err(CursorSdkError::BridgeNotFound(format!(
        "install the sidecar next to grok (or run the fetch script), or set {BRIDGE_BIN_ENV}"
    )))
}

fn dunce_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    dunce::canonicalize(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn touch_exec(path: &Path) {
        fs::write(path, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn env_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("cursor-sdk-bridge");
        touch_exec(&bin);
        let got = discover_bridge_bin_with(
            Some(bin.clone()),
            Some(dir.path().join("grok")),
            dir.path().join("unused-home"),
            |_| panic!("PATH should not be consulted"),
        )
        .unwrap();
        assert_eq!(got, bin);
    }

    #[test]
    fn sibling_of_current_exe() {
        let dir = tempfile::tempdir().unwrap();
        let grok = dir.path().join("grok");
        touch_exec(&grok);
        let bridge = dir.path().join("cursor-sdk-bridge");
        touch_exec(&bridge);
        let got =
            discover_bridge_bin_with(None, Some(grok), dir.path().join("unused-home"), |_| {
                panic!("PATH should not be consulted")
            })
            .unwrap();
        assert_eq!(
            dunce::canonicalize(&got).unwrap(),
            dunce::canonicalize(&bridge).unwrap()
        );
    }

    #[test]
    fn grok_bin_dir_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(home.join("bin")).unwrap();
        let bridge = home.join("bin").join("cursor-sdk-bridge");
        touch_exec(&bridge);
        let got =
            discover_bridge_bin_with(None, Some(dir.path().join("missing-grok")), home, |_| {
                panic!("PATH should not be consulted")
            })
            .unwrap();
        assert_eq!(got, bridge);
    }

    #[test]
    fn path_last() {
        let dir = tempfile::tempdir().unwrap();
        let path_bin = dir.path().join("on-path");
        touch_exec(&path_bin);
        let got = discover_bridge_bin_with(
            None,
            Some(dir.path().join("missing-grok")),
            dir.path().join("empty-home"),
            |_| Some(path_bin.clone()),
        )
        .unwrap();
        assert_eq!(got, path_bin);
    }

    #[test]
    fn missing_is_actionable() {
        let dir = tempfile::tempdir().unwrap();
        let err = discover_bridge_bin_with(
            None,
            Some(dir.path().join("missing-grok")),
            dir.path().join("empty-home"),
            |_| None,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(BRIDGE_BIN_ENV), "{msg}");
    }

    #[test]
    fn resolve_prefers_current_exe_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("xai-grok-pager");
        touch_exec(&exe);
        let got = resolve_grok_bin_with(Some(exe.clone()), |_| {
            panic!("PATH should not be consulted")
        });
        assert_eq!(
            dunce::canonicalize(&got).unwrap(),
            dunce::canonicalize(&exe).unwrap()
        );
    }

    #[test]
    fn resolve_falls_back_to_path_when_current_exe_missing() {
        let dir = tempfile::tempdir().unwrap();
        let on_path = dir.path().join("grok");
        touch_exec(&on_path);
        let got =
            resolve_grok_bin_with(Some(dir.path().join("missing")), |_| Some(on_path.clone()));
        assert_eq!(got, on_path.to_string_lossy());
    }

    #[test]
    fn resolve_falls_back_to_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        let got = resolve_grok_bin_with(Some(dir.path().join("missing")), |_| None);
        assert_eq!(got, "grok");
    }
}
