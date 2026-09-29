//! Same-user local transport. No credentials or document data are stored here.
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Seek, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};
use tokio::net::{UnixListener, UnixStream};

pub fn endpoint() -> PathBuf {
    // Never accepted by release binaries. Used only by isolated native tests.
    #[cfg(debug_assertions)]
    if let Some(directory) = std::env::var_os("SIXA_MCP_TEST_DIRECTORY") {
        return PathBuf::from(directory).join("mcp.sock");
    }
    PathBuf::from(format!("/private/tmp/cn.shierkeji.sixa.{}", uid())).join("mcp.sock")
}

pub fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "私匣本机通信目录、文件或连接的权限不安全",
    )
}

fn directory(path: &Path, create: bool) -> io::Result<&Path> {
    let parent = path.parent().ok_or_else(denied)?;
    if create {
        match DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
    }
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o777 != 0o700 {
        return Err(denied());
    }
    Ok(parent)
}

/// An advisory lock held until drop; the lock file must never be unlinked.
pub struct Lock(File);

impl Lock {
    /// Shared cooldown across AI clients, stored in the already locked private file.
    pub fn begin_launch(&mut self, cooldown: std::time::Duration) -> io::Result<bool> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        self.0.rewind()?;
        let mut value = String::new();
        Read::by_ref(&mut self.0)
            .take(32)
            .read_to_string(&mut value)?;
        if let Ok(previous) = value.parse::<u64>() {
            if now.abs_diff(previous) < cooldown.as_secs() {
                return Ok(false);
            }
        }
        self.0.rewind()?;
        self.0.set_len(0)?;
        write!(self.0, "{now}")?;
        self.0.flush()?;
        Ok(true)
    }
}

pub fn try_lock(path: &Path, name: &str) -> io::Result<Lock> {
    let parent = directory(path, true)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent.join(name))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != uid() || metadata.mode() & 0o777 != 0o600 {
        return Err(denied());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Lock(file))
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub struct Server {
    pub listener: UnixListener,
    path: PathBuf,
    inode: u64,
    _lock: Lock,
}

impl Server {
    pub fn bind(path: &Path) -> io::Result<Self> {
        let lock = try_lock(path, "server.lock")?;
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() || metadata.uid() != uid() {
                    return Err(denied());
                }
                // Do not remove a live endpoint, even if another server did not take our lock.
                match std::os::unix::net::UnixStream::connect(path) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "私匣通信服务已运行",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => (),
                    Err(error) => return Err(error),
                }
                fs::remove_file(path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        let listener = std::os::unix::net::UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let inode = fs::symlink_metadata(path)?.ino();
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener: UnixListener::from_std(listener)?,
            path: path.to_owned(),
            inode,
            _lock: lock,
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.ino() == self.inode && m.file_type().is_socket() && m.uid() == uid())
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub fn check_peer(stream: &UnixStream) -> io::Result<()> {
    if stream.peer_cred()?.uid() != uid() {
        return Err(denied());
    }
    Ok(())
}

pub async fn connect(path: &Path) -> io::Result<UnixStream> {
    directory(path, false)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != uid()
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(denied());
    }
    let stream = UnixStream::connect(path).await?;
    check_peer(&stream)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        // macOS user temp paths can exceed sockaddr_un.sun_path's limit.
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        let path = root.path().join("private/mcp.sock");
        (root, path)
    }

    #[tokio::test]
    async fn same_user_round_trip_and_cleanup() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (_root, path) = fixture();
        let server = Server::bind(&path).unwrap();
        assert!(Server::bind(&path).is_err());
        let mut client = connect(&path).await.unwrap();
        let (mut stream, _) = server.listener.accept().await.unwrap();
        check_peer(&stream).unwrap();
        client.write_u32_le(42).await.unwrap();
        assert_eq!(stream.read_u32_le().await.unwrap(), 42);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        drop(server);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn stale_socket_is_recovered_but_other_files_are_not_deleted() {
        let (_root, path) = fixture();
        directory(&path, true).unwrap();
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        drop(Server::bind(&path).unwrap());
        fs::write(&path, b"keep").unwrap();
        assert!(Server::bind(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep");
    }

    #[tokio::test]
    async fn rejects_symlinks_and_public_directories() {
        let (root, path) = fixture();
        directory(&path, true).unwrap();
        std::os::unix::fs::symlink(root.path().join("target"), &path).unwrap();
        assert!(Server::bind(&path).is_err());
        assert!(connect(&path).await.is_err());
        fs::remove_file(&path).unwrap();
        fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Server::bind(&path).is_err());
        assert!(connect(&path).await.is_err());
    }

    #[test]
    fn locks_are_exclusive_and_reusable() {
        let (_root, path) = fixture();
        let lock = try_lock(&path, "launch.lock").unwrap();
        assert!(try_lock(&path, "launch.lock").is_err());
        drop(lock);
        assert!(try_lock(&path, "launch.lock").is_ok());
    }

    #[test]
    fn launch_cooldown_survives_lock_release() {
        let (_root, path) = fixture();
        let cooldown = std::time::Duration::from_secs(30);
        let mut first = try_lock(&path, "launch.lock").unwrap();
        assert!(first.begin_launch(cooldown).unwrap());
        drop(first);
        let mut second = try_lock(&path, "launch.lock").unwrap();
        assert!(!second.begin_launch(cooldown).unwrap());
    }

    #[tokio::test]
    async fn refuses_symlinked_directory_lock_and_live_socket() {
        let (root, path) = fixture();
        let parent = path.parent().unwrap();
        std::os::unix::fs::symlink(root.path(), parent).unwrap();
        assert!(Server::bind(&path).is_err());
        fs::remove_file(parent).unwrap();
        directory(&path, true).unwrap();
        std::os::unix::fs::symlink(root.path().join("target"), parent.join("server.lock")).unwrap();
        assert!(Server::bind(&path).is_err());
        assert!(!root.path().join("target").exists());
        fs::remove_file(parent.join("server.lock")).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(Server::bind(&path).is_err());
        assert!(path.exists());
        drop(listener);
    }
}
