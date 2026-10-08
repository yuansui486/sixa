//! Short-lived installation lease, shared by the desktop and bundled MCP.
//! Stored outside the installation so replacing the bundle cannot remove it.
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
struct Lease {
    version: String,
    expires: u64,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("LocalDesensitization"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|p| PathBuf::from(p).join("Library/Application Support/LocalDesensitization"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}
fn path(root: &Path) -> PathBuf {
    root.join("cache/app-update-lease.json")
}
fn read(root: &Path) -> Option<Lease> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path(root))
        .ok()?
        .take(1024)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}
pub fn active_at(root: &Path) -> bool {
    read(root).is_some_and(|v| v.expires > now() && v.expires <= now() + 600)
}
pub fn active() -> bool {
    root().is_some_and(|root| active_at(&root))
}
pub fn begin(root: &Path, version: &str) -> io::Result<()> {
    use std::io::Write;
    let target = path(root);
    std::fs::create_dir_all(target.parent().unwrap())?;
    if !active_at(root) {
        clear(root);
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    serde_json::to_writer(
        &mut file,
        &Lease {
            version: version.into(),
            expires: now() + 600,
        },
    )?;
    file.flush()?;
    file.sync_all()
}
pub fn clear(root: &Path) {
    let _ = std::fs::remove_file(path(root));
}
pub fn finish_startup(root: &Path, version: &str) {
    if read(root).is_some_and(|v| v.version == version || v.expires <= now()) {
        clear(root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_new_version_clears_live_lease() {
        let temp = tempfile::tempdir().unwrap();
        begin(temp.path(), "1.0.9").unwrap();
        assert!(active_at(temp.path()));
        finish_startup(temp.path(), "1.0.8");
        assert!(active_at(temp.path()));
        finish_startup(temp.path(), "1.0.9");
        assert!(!active_at(temp.path()));
    }
    #[test]
    fn stale_lease_does_not_disable_mcp_forever() {
        let temp = tempfile::tempdir().unwrap();
        begin(temp.path(), "1.0.9").unwrap();
        std::fs::write(path(temp.path()), br#"{"version":"1.0.9","expires":1}"#).unwrap();
        assert!(!active_at(temp.path()));
        finish_startup(temp.path(), "1.0.8");
        assert!(!path(temp.path()).exists());
    }
}
