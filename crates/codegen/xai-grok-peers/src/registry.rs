//! Lock-guarded peer registry under `~/.grok/peers/`.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::naming::sanitize_peer_name;
use crate::{PEERS_DIR, peers_dir, peers_dir_in};

const INDEX_FILENAME: &str = "index.json";
const LOCK_FILENAME: &str = "peers.lock";
const TMP_FILENAME: &str = "index.json.tmp";

/// Live peer advertised for cross-session messaging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerRecord {
    pub session_id: String,
    pub name: String,
    pub cwd: String,
    pub pid: u32,
    pub inbox_path: String,
    pub updated_at: DateTime<Utc>,
}

// -- Public API -------------------------------------------------------------

pub fn register(peer: PeerRecord) -> io::Result<PeerRecord> {
    register_in(&xai_grok_config::grok_home(), peer)
}

pub fn unregister(session_id: &str) -> io::Result<()> {
    unregister_in(&xai_grok_config::grok_home(), session_id)
}

pub fn update_name(session_id: &str, new_name: &str) -> io::Result<Option<PeerRecord>> {
    update_name_in(&xai_grok_config::grok_home(), session_id, new_name)
}

pub fn list_live() -> io::Result<Vec<PeerRecord>> {
    list_live_in(&xai_grok_config::grok_home())
}

pub fn register_in(root: &Path, mut peer: PeerRecord) -> io::Result<PeerRecord> {
    peer.name = sanitize_peer_name(&peer.name);
    if peer.name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "peer name must not be empty",
        ));
    }
    with_locked_state(root, |peers| {
        peers.retain(|p| p.session_id != peer.session_id || is_pid_alive(p.pid));
        let taken: HashSet<String> = peers
            .iter()
            .filter(|p| p.session_id != peer.session_id && is_pid_alive(p.pid))
            .map(|p| p.name.clone())
            .collect();
        peer.name = crate::naming::allocate_live_name(&peer.name, &taken);
        peer.updated_at = Utc::now();
        peers.retain(|p| p.session_id != peer.session_id);
        peers.push(peer.clone());
        peer
    })
}

pub fn unregister_in(root: &Path, session_id: &str) -> io::Result<()> {
    with_locked_state(root, |peers| {
        peers.retain(|p| p.session_id != session_id);
    })
}

pub fn update_name_in(
    root: &Path,
    session_id: &str,
    new_name: &str,
) -> io::Result<Option<PeerRecord>> {
    let sanitized = sanitize_peer_name(new_name);
    if sanitized.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "peer name must not be empty",
        ));
    }
    with_locked_state(root, |peers| {
        peers.retain(|p| is_pid_alive(p.pid) || p.session_id == session_id);
        let taken: HashSet<String> = peers
            .iter()
            .filter(|p| p.session_id != session_id && is_pid_alive(p.pid))
            .map(|p| p.name.clone())
            .collect();
        let Some(idx) = peers.iter().position(|p| p.session_id == session_id) else {
            return None;
        };
        let name = crate::naming::allocate_live_name(&sanitized, &taken);
        peers[idx].name = name;
        peers[idx].updated_at = Utc::now();
        Some(peers[idx].clone())
    })
}

pub fn list_live_in(root: &Path) -> io::Result<Vec<PeerRecord>> {
    with_locked_state(root, |peers| {
        let (alive, _dead): (Vec<_>, Vec<_>) =
            peers.drain(..).partition(|p| is_pid_alive(p.pid));
        *peers = alive.clone();
        alive
    })
}

/// Path helpers for callers that need the peers directory without locking.
#[allow(dead_code)]
pub fn peers_root() -> PathBuf {
    peers_dir()
}

#[allow(dead_code)]
pub fn peers_root_in(root: &Path) -> PathBuf {
    peers_dir_in(root)
}

// -- Internal ---------------------------------------------------------------

fn with_locked_state<F, R>(root: &Path, mutate: F) -> io::Result<R>
where
    F: FnOnce(&mut Vec<PeerRecord>) -> R,
{
    let dir = root.join(PEERS_DIR);
    let lock_path = dir.join(LOCK_FILENAME);
    let data_path = dir.join(INDEX_FILENAME);
    let tmp_path = dir.join(TMP_FILENAME);

    fs::create_dir_all(&dir)?;
    let lock_file = open_lock_file(&lock_path)?;
    lock_file.lock_exclusive()?;

    let mut peers = read_data_file(&data_path)?;
    let result = mutate(&mut peers);
    write_data_file_atomic(&tmp_path, &data_path, &peers)?;

    let _ = lock_file.unlock();
    Ok(result)
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn read_data_file(path: &Path) -> io::Result<Vec<PeerRecord>> {
    match fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(Vec::new()),
        Ok(bytes) => match serde_json::from_slice::<Vec<PeerRecord>>(&bytes) {
            Ok(peers) => Ok(peers),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "peers index.json is corrupted, starting empty"
                );
                Ok(Vec::new())
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

fn write_data_file_atomic(
    tmp_path: &Path,
    data_path: &Path,
    peers: &[PeerRecord],
) -> io::Result<()> {
    let json = serde_json::to_string_pretty(peers)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(tmp_path, json.as_bytes())?;
    fs::rename(tmp_path, data_path).inspect_err(|_| {
        let _ = fs::remove_file(tmp_path);
    })
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let pid_i = match i32::try_from(pid) {
            Ok(p) if p > 0 => p,
            _ => return false,
        };
        let ret = unsafe { libc::kill(pid_i as libc::pid_t, 0) };
        if ret == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
        match handle {
            Ok(h) => {
                let _ = unsafe { CloseHandle(h) };
                true
            }
            Err(_) => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(session_id: &str, name: &str) -> PeerRecord {
        PeerRecord {
            session_id: session_id.into(),
            name: name.into(),
            cwd: "/tmp/proj".into(),
            pid: std::process::id(),
            inbox_path: format!("/tmp/{session_id}.sock"),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn register_list_unregister_roundtrip() {
        let dir = tempdir().unwrap();
        let a = register_in(dir.path(), sample("s1", "api")).unwrap();
        assert_eq!(a.name, "api");
        let listed = list_live_in(dir.path()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, "s1");
        unregister_in(dir.path(), "s1").unwrap();
        assert!(list_live_in(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn register_renames_on_collision() {
        let dir = tempdir().unwrap();
        let _ = register_in(dir.path(), sample("s1", "api")).unwrap();
        let b = register_in(dir.path(), sample("s2", "api")).unwrap();
        assert_eq!(b.name, "api-2");
    }

    #[test]
    fn update_name_allocates_variant() {
        let dir = tempdir().unwrap();
        let _ = register_in(dir.path(), sample("s1", "api")).unwrap();
        let _ = register_in(dir.path(), sample("s2", "other")).unwrap();
        let updated = update_name_in(dir.path(), "s2", "api").unwrap().unwrap();
        assert_eq!(updated.name, "api-2");
    }
}
