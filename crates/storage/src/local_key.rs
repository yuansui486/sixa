//! macOS local master key. Same-user processes are inside this trust boundary.
//! Only a legacy profile without a local key may request Keychain migration.
use domain::{Error, Result};
use rand::RngCore;
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
};
use zeroize::Zeroizing;

const KEY_FILE: &str = "internal-key-v1";
const MARKER: &str = "db/local-key-v1";

fn unsafe_permissions() -> Error {
    Error::State(
        "本机密钥文件或目录权限异常。请保留现有文件并联系支持，不要删除密钥或重新生成密钥。".into(),
    )
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn private_directory(root: &Path) -> Result<File> {
    fs::create_dir_all(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(unsafe_permissions());
    }
    let directory = root.join("secrets");
    match DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory)?;
    let metadata = file.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
        return Err(unsafe_permissions());
    }
    Ok(file)
}

fn private_file(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(unsafe_permissions());
    }
    Ok(file)
}

fn decode(bytes: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    bytes.try_into().map(Zeroizing::new).map_err(|_| Error::State(
        "加密密钥长度异常，已停止启动以保护历史数据。请恢复密钥备份或联系支持，不要删除现有文件。".into(),
    ))
}

fn record_local_key(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join("db"))?;
    // Separate from secrets so removal of that directory cannot silently reset a profile.
    if !exists(&root.join(MARKER))? {
        crate::atomic_write(&root.join(MARKER), b"local-key-v1\n")?;
    }
    Ok(())
}

fn legacy_profile(root: &Path) -> Result<bool> {
    for relative in ["db/app.sqlite3", "db/auth-state.enc"] {
        if exists(&root.join(relative))? {
            return Ok(true);
        }
    }
    match fs::read_dir(root.join("tasks")) {
        Ok(mut entries) => Ok(entries.next().transpose()?.is_some()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn load_or_migrate(
    root: &Path,
    legacy: impl FnOnce() -> Result<Zeroizing<Vec<u8>>>,
) -> Result<Zeroizing<[u8; 32]>> {
    let directory = private_directory(root)?;
    let secrets = root.join("secrets");
    let lock = private_file(&secrets.join("key.lock"), true)?;
    // Do not block forever behind a migration that is waiting for user interaction.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::State(
            "另一私匣进程正在准备密钥，请完成该窗口的提示后重新打开。".into(),
        ));
    }
    let path = secrets.join(KEY_FILE);
    if exists(&path)? {
        let mut bytes = Zeroizing::new(Vec::with_capacity(33));
        private_file(&path, false)?
            .take(33)
            .read_to_end(&mut bytes)?;
        let key = decode(&bytes)?;
        record_local_key(root)?;
        return Ok(key);
    }
    if exists(&root.join(MARKER))? {
        return Err(Error::State("本机密钥文件丢失，已停止启动。请从备份恢复 secrets/internal-key-v1；不会使用旧钥匙串或新密钥覆盖历史数据。".into()));
    }
    let key = if legacy_profile(root)? {
        let key = decode(&legacy()?)?;
        // Verify migration against existing encrypted login state when available.
        let auth = root.join("db/auth-state.enc");
        if exists(&auth)? {
            let encrypted = fs::read(auth)?;
            let _plain = Zeroizing::new(
                crate::vault::open(&key, &encrypted, b"sixa-auth-state-v1").map_err(|_| {
                    Error::State("旧密钥无法解密现有登录数据，迁移已停止，原文件未修改。".into())
                })?,
            );
        }
        key
    } else {
        let mut key = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(key.as_mut());
        key
    };
    let mut temporary = tempfile::NamedTempFile::new_in(&secrets)?;
    temporary.write_all(key.as_ref())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(&path)
        .map_err(|e| Error::Io(e.to_string()))?;
    directory.sync_all()?;
    record_local_key(root)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn no_keychain() -> Result<Zeroizing<Vec<u8>>> {
        panic!("must not access Keychain")
    }
    fn old_profile(root: &Path, key: &[u8; 32]) -> Vec<u8> {
        fs::create_dir_all(root.join("db")).unwrap();
        let bytes = crate::vault::seal(key, b"old session", b"sixa-auth-state-v1").unwrap();
        fs::write(root.join("db/auth-state.enc"), &bytes).unwrap();
        bytes
    }

    #[test]
    fn fresh_profile_and_restart_never_access_keychain() {
        let root = tempfile::tempdir().unwrap();
        let key = load_or_migrate(root.path(), no_keychain).unwrap();
        assert_eq!(
            key.as_ref(),
            load_or_migrate(root.path(), no_keychain).unwrap().as_ref()
        );
        assert_eq!(
            fs::metadata(root.path().join("secrets")).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.path().join("secrets/internal-key-v1"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert!(root.path().join(MARKER).is_file());
    }

    #[test]
    fn migration_preserves_key_and_ciphertext_then_skips_keychain() {
        let root = tempfile::tempdir().unwrap();
        let original = old_profile(root.path(), &[42; 32]);
        let key = load_or_migrate(root.path(), || Ok(Zeroizing::new(vec![42; 32]))).unwrap();
        assert_eq!(key.as_ref(), &[42; 32]);
        assert_eq!(
            fs::read(root.path().join("db/auth-state.enc")).unwrap(),
            original
        );
        assert_eq!(
            load_or_migrate(root.path(), no_keychain).unwrap().as_ref(),
            &[42; 32]
        );
        let saved = crate::vault::seal(&key, b"task", b"task-id").unwrap();
        assert_eq!(
            crate::vault::open(
                &load_or_migrate(root.path(), no_keychain).unwrap(),
                &saved,
                b"task-id"
            )
            .unwrap(),
            b"task"
        );
    }

    #[test]
    fn denied_missing_wrong_or_corrupt_legacy_key_never_resets_data() {
        for value in [None, Some(vec![1; 31]), Some(vec![2; 32])] {
            let root = tempfile::tempdir().unwrap();
            let original = old_profile(root.path(), &[42; 32]);
            assert!(
                load_or_migrate(root.path(), || value
                    .map(Zeroizing::new)
                    .ok_or_else(|| Error::State("denied or missing".into())))
                .is_err()
            );
            assert!(!root.path().join("secrets/internal-key-v1").exists());
            assert!(!root.path().join(MARKER).exists());
            assert_eq!(
                fs::read(root.path().join("db/auth-state.enc")).unwrap(),
                original
            );
            // User authorizes a subsequent attempt; the original key remains usable.
            assert!(load_or_migrate(root.path(), || Ok(Zeroizing::new(vec![42; 32]))).is_ok());
        }
    }

    #[test]
    fn damaged_or_deleted_local_key_never_falls_back_to_keychain() {
        let root = tempfile::tempdir().unwrap();
        load_or_migrate(root.path(), no_keychain).unwrap();
        let path = root.path().join("secrets/internal-key-v1");
        fs::write(&path, b"broken").unwrap();
        assert!(load_or_migrate(root.path(), no_keychain).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
        fs::remove_file(&path).unwrap();
        assert!(load_or_migrate(root.path(), no_keychain).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn rejects_public_permissions_symlinks_and_hardlinks() {
        let root = tempfile::tempdir().unwrap();
        load_or_migrate(root.path(), no_keychain).unwrap();
        let path = root.path().join("secrets/internal-key-v1");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_or_migrate(root.path(), no_keychain).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let copy = root.path().join("hardlink");
        fs::hard_link(&path, &copy).unwrap();
        assert!(load_or_migrate(root.path(), no_keychain).is_err());
        fs::remove_file(copy).unwrap();
        let original = root.path().join("original");
        fs::rename(&path, &original).unwrap();
        symlink(&original, &path).unwrap();
        assert!(load_or_migrate(root.path(), no_keychain).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(original, &path).unwrap();
        fs::set_permissions(
            root.path().join("secrets"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(load_or_migrate(root.path(), no_keychain).is_err());
    }

    #[test]
    fn rejects_concurrent_migration_without_waiting_for_another_prompt() {
        let root = tempfile::tempdir().unwrap();
        old_profile(root.path(), &[42; 32]);
        load_or_migrate(root.path(), || {
            assert!(load_or_migrate(root.path(), no_keychain).is_err());
            Ok(Zeroizing::new(vec![42; 32]))
        })
        .unwrap();
    }

    #[test]
    #[ignore = "uses a disposable macOS CI user's Keychain"]
    fn native_keychain_migration() {
        assert_eq!(std::env::var("CI").as_deref(), Ok("true"));
        let root = tempfile::tempdir().unwrap();
        let service = format!("Sixa-CI-{}", uuid::Uuid::new_v4());
        let entry = keyring::Entry::new(&service, "migration-fixture").unwrap();
        struct Cleanup(keyring::Entry);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.delete_credential();
            }
        }
        let cleanup = Cleanup(entry);
        cleanup.0.set_secret(&[73; 32]).unwrap();
        let encrypted = old_profile(root.path(), &[73; 32]);
        let key = load_or_migrate(root.path(), || {
            Ok(Zeroizing::new(cleanup.0.get_secret().unwrap()))
        })
        .unwrap();
        assert_eq!(key.as_ref(), &[73; 32]);
        drop(cleanup);
        let restored = load_or_migrate(root.path(), no_keychain).unwrap();
        assert_eq!(
            crate::vault::open(&restored, &encrypted, b"sixa-auth-state-v1").unwrap(),
            b"old session"
        );
    }
}
