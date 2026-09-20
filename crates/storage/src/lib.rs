pub mod vault;
use domain::{AppSettings, Error, ErrorInfo, Policy, Result, Rule, TaskMeta};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;
use zeroize::Zeroizing;

const SCHEMA_VERSION: i64 = 2;

pub struct Store {
    db: Connection,
    pub root: PathBuf,
    key: Zeroizing<[u8; 32]>,
}
fn db_err(e: rusqlite::Error) -> Error {
    Error::Io(e.to_string())
}
fn json_err(e: serde_json::Error) -> Error {
    Error::Invalid(e.to_string())
}
impl Store {
    pub fn open(root: &Path, key: Zeroizing<[u8; 32]>) -> Result<Self> {
        for p in ["db", "tasks", "models", "cache", "logs"] {
            std::fs::create_dir_all(root.join(p))?;
        }
        let mut db = Connection::open(root.join("db/app.sqlite3")).map_err(db_err)?;
        db.busy_timeout(Duration::from_secs(10)).map_err(db_err)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON;",
        )
        .map_err(db_err)?;
        migrate_database(&mut db)?;
        let store = Self {
            db,
            root: root.into(),
            key,
        };
        for mut meta in store.tasks()? {
            if matches!(
                meta.state,
                domain::TaskState::Queued
                    | domain::TaskState::Analyzing
                    | domain::TaskState::Processing
            ) {
                meta.state = domain::TaskState::Failed;
                meta.error = Some("上次运行意外中断，请重新创建任务".into());
                meta.error_info = Some(ErrorInfo::new(
                    "INTERRUPTED",
                    "任务意外中断",
                    "应用上次退出时任务仍在运行",
                    "请从原文件重新创建任务",
                    true,
                ));
                store.save_meta(&meta)?;
            }
        }
        Ok(store)
    }
    pub fn task_dir(&self, id: Uuid) -> PathBuf {
        self.root.join("tasks").join(id.to_string())
    }
    pub fn save_meta(&self, meta: &TaskMeta) -> Result<()> {
        self.db.execute("INSERT INTO tasks VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata", params![meta.id.to_string(),serde_json::to_string(meta).map_err(json_err)?]).map_err(db_err)?;
        Ok(())
    }
    pub fn task(&self, id: Uuid) -> Result<Option<TaskMeta>> {
        let row = self
            .db
            .query_row(
                "SELECT metadata FROM tasks WHERE id=?1",
                [id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_err)?;
        let Some(metadata) = row else {
            return Ok(None);
        };
        match parse_task_record(&id.to_string(), &metadata) {
            Ok(meta) => Ok(Some(meta)),
            Err(error) => {
                self.quarantine_task(&id.to_string(), &metadata, &error)?;
                Err(Error::Invalid("任务元数据已损坏并被隔离".into()))
            }
        }
    }
    pub fn tasks(&self) -> Result<Vec<TaskMeta>> {
        let records = {
            let mut stmt = self
                .db
                .prepare("SELECT id, metadata FROM tasks")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(db_err)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)?
        };
        let mut result = Vec::new();
        for (id, metadata) in records {
            match parse_task_record(&id, &metadata) {
                Ok(meta) => result.push(meta),
                Err(error) => self.quarantine_task(&id, &metadata, &error)?,
            }
        }
        result.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        Ok(result)
    }
    pub fn save_payload<T: serde::Serialize>(&self, id: Uuid, payload: &T) -> Result<()> {
        let dir = self.task_dir(id);
        std::fs::create_dir_all(&dir)?;
        let bytes = Zeroizing::new(serde_json::to_vec(payload).map_err(json_err)?);
        let encrypted = vault::seal(&self.key, &bytes, id.as_bytes())?;
        atomic_write(&dir.join("analysis.enc"), &encrypted)?;
        self.refresh_storage_bytes(id)
    }
    pub fn payload<T: serde::de::DeserializeOwned>(&self, id: Uuid) -> Result<T> {
        let bytes = std::fs::read(self.task_dir(id).join("analysis.enc"))?;
        let plain = Zeroizing::new(vault::open(&self.key, &bytes, id.as_bytes())?);
        serde_json::from_slice(&plain).map_err(json_err)
    }
    pub fn save_review<T: serde::Serialize>(&self, id: Uuid, review: &T) -> Result<()> {
        self.save_sidecar(id, "review.enc", review)
    }
    pub fn review<T: serde::de::DeserializeOwned>(&self, id: Uuid) -> Result<Option<T>> {
        self.sidecar(id, "review.enc")
    }
    pub fn save_output(&self, id: Uuid, output: &[u8]) -> Result<()> {
        let dir = self.task_dir(id);
        std::fs::create_dir_all(&dir)?;
        let encrypted = vault::seal(&self.key, output, &sidecar_aad(id, "output.enc"))?;
        atomic_write(&dir.join("output.enc"), &encrypted)?;
        self.refresh_storage_bytes(id)
    }
    pub fn output(&self, id: Uuid) -> Result<Option<Vec<u8>>> {
        let path = self.task_dir(id).join("output.enc");
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)?;
        vault::open(&self.key, &bytes, &sidecar_aad(id, "output.enc")).map(Some)
    }
    pub fn remove_output(&self, id: Uuid) -> Result<()> {
        let path = self.task_dir(id).join("output.enc");
        if path.exists() {
            std::fs::remove_file(path)?;
            self.refresh_storage_bytes(id)?;
        }
        Ok(())
    }
    fn save_sidecar<T: serde::Serialize>(&self, id: Uuid, name: &str, value: &T) -> Result<()> {
        let dir = self.task_dir(id);
        std::fs::create_dir_all(&dir)?;
        let bytes = Zeroizing::new(serde_json::to_vec(value).map_err(json_err)?);
        let encrypted = vault::seal(&self.key, &bytes, &sidecar_aad(id, name))?;
        atomic_write(&dir.join(name), &encrypted)?;
        self.refresh_storage_bytes(id)
    }
    fn sidecar<T: serde::de::DeserializeOwned>(&self, id: Uuid, name: &str) -> Result<Option<T>> {
        let path = self.task_dir(id).join(name);
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)?;
        let plain = Zeroizing::new(vault::open(&self.key, &bytes, &sidecar_aad(id, name))?);
        serde_json::from_slice(&plain).map(Some).map_err(json_err)
    }
    pub fn delete(&self, id: Uuid) -> Result<()> {
        let path = self.task_dir(id);
        if path.exists() {
            std::fs::remove_dir_all(path)?;
        }
        self.db
            .execute("DELETE FROM tasks WHERE id=?1", [id.to_string()])
            .map_err(db_err)?;
        Ok(())
    }
    fn refresh_storage_bytes(&self, id: Uuid) -> Result<()> {
        let Some(mut meta) = self.task(id)? else {
            return Ok(());
        };
        meta.storage_bytes = directory_size(&self.task_dir(id))?;
        self.save_meta(&meta)
    }
    fn quarantine_task(&self, id: &str, metadata: &str, reason: &str) -> Result<()> {
        let transaction = self.db.unchecked_transaction().map_err(db_err)?;
        transaction
            .execute(
                "INSERT OR REPLACE INTO corrupt_tasks(id,metadata,error,quarantined_at) VALUES(?1,?2,?3,?4)",
                params![id, metadata, reason, unix_time()],
            )
            .map_err(db_err)?;
        transaction
            .execute("DELETE FROM tasks WHERE id=?1", [id])
            .map_err(db_err)?;
        transaction.commit().map_err(db_err)
    }
    fn config<T: serde::de::DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>> {
        let mut stmt = self
            .db
            .prepare("SELECT data FROM config WHERE kind=?1 ORDER BY id")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([kind], |r| r.get::<_, String>(0))
            .map_err(db_err)?;
        rows.map(|r| serde_json::from_str(&r.map_err(db_err)?).map_err(json_err))
            .collect()
    }
    fn put<T: serde::Serialize>(&self, kind: &str, id: &str, data: &T) -> Result<()> {
        self.db.execute("INSERT INTO config VALUES(?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET data=excluded.data",params![kind,id,serde_json::to_string(data).map_err(json_err)?]).map_err(db_err)?;
        Ok(())
    }
    pub fn rules(&self) -> Result<Vec<Rule>> {
        self.config("rule")
    }
    pub fn policies(&self) -> Result<Vec<Policy>> {
        self.config("policy")
    }
    pub fn save_rule(&mut self, rule: &Rule) -> Result<()> {
        let tx = self.db.transaction().map_err(db_err)?;
        tx.execute("INSERT INTO config VALUES('rule',?1,?2) ON CONFLICT(kind,id) DO UPDATE SET data=excluded.data",params![rule.id.to_string(),serde_json::to_string(rule).map_err(json_err)?]).map_err(db_err)?;
        let policy = Policy {
            entity_type: rule.entity_type.clone(),
            replacement: "已脱敏".into(),
        };
        if !matches!(
            rule.entity_type.as_str(),
            "PERSON"
                | "ORGANIZATION"
                | "LOCATION"
                | "GPE"
                | "ADDRESS"
                | "PHONE"
                | "EMAIL"
                | "ID_CARD"
                | "BANK_CARD"
        ) {
            tx.execute(
                "INSERT OR IGNORE INTO config VALUES('policy',?1,?2)",
                params![
                    rule.entity_type,
                    serde_json::to_string(&policy).map_err(json_err)?
                ],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }
    pub fn save_policy(&self, p: &Policy) -> Result<()> {
        self.put("policy", &p.entity_type, p)
    }
    pub fn settings(&self) -> Result<AppSettings> {
        Ok(self
            .config::<AppSettings>("settings")?
            .into_iter()
            .next()
            .unwrap_or_default())
    }
    pub fn save_settings(&self, settings: AppSettings) -> Result<()> {
        let settings = settings.validate()?;
        self.put("settings", "app", &settings)
    }
    pub fn delete_rule(&self, id: Uuid) -> Result<()> {
        self.db
            .execute(
                "DELETE FROM config WHERE kind='rule' AND id=?1",
                [id.to_string()],
            )
            .map_err(db_err)?;
        Ok(())
    }
}

fn sidecar_aad(id: Uuid, name: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(16 + name.len());
    aad.extend_from_slice(id.as_bytes());
    aad.extend_from_slice(name.as_bytes());
    aad
}

fn migrate_database(db: &mut Connection) -> Result<()> {
    let version = db
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(db_err)?;
    if version > SCHEMA_VERSION {
        return Err(Error::Invalid(format!(
            "数据库版本 {version} 高于应用支持的版本 {SCHEMA_VERSION}"
        )));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let transaction = db.transaction().map_err(db_err)?;
    let mut current = version;
    while current < SCHEMA_VERSION {
        match current {
            0 => transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, metadata TEXT NOT NULL);
                     CREATE TABLE IF NOT EXISTS config(kind TEXT NOT NULL, id TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY(kind,id));",
                )
                .map_err(db_err)?,
            1 => transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS corrupt_tasks(
                       id TEXT PRIMARY KEY,
                       metadata TEXT NOT NULL,
                       error TEXT NOT NULL,
                       quarantined_at INTEGER NOT NULL
                     );",
                )
                .map_err(db_err)?,
            _ => return Err(Error::Invalid(format!("没有数据库版本 {current} 的迁移"))),
        }
        current += 1;
    }
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(db_err)?;
    transaction.commit().map_err(db_err)
}

fn parse_task_record(id: &str, metadata: &str) -> std::result::Result<TaskMeta, String> {
    let meta = serde_json::from_str::<TaskMeta>(metadata).map_err(|error| error.to_string())?;
    if meta.id.to_string() != id {
        return Err("记录 ID 与元数据 ID 不一致".into());
    }
    Ok(meta)
}

fn directory_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                total = total
                    .checked_add(entry.metadata()?.len())
                    .ok_or_else(|| Error::Io("任务占用空间计算溢出".into()))?;
            }
        }
    }
    Ok(total)
}

fn unix_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("输出路径无父目录".into()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| Error::Io(e.to_string()))?;
    Ok(())
}
pub fn credential_key() -> Result<Zeroizing<[u8; 32]>> {
    use rand::RngCore;
    let entry = keyring::Entry::new("LocalDesensitization", "internal-key-v1")
        .map_err(|e| Error::Io(e.to_string()))?;
    match entry.get_secret() {
        Ok(bytes) => {
            let bytes = Zeroizing::new(bytes);
            let key: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| Error::Authentication)?;
            Ok(Zeroizing::new(key))
        }
        Err(keyring::Error::NoEntry) => {
            let mut key = Zeroizing::new([0u8; 32]);
            rand::rngs::OsRng.fill_bytes(key.as_mut());
            entry
                .set_secret(key.as_ref())
                .map_err(|e| Error::Io(e.to_string()))?;
            Ok(key)
        }
        Err(e) => Err(Error::Io(format!("无法读取 Windows 凭据：{e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_rule_creates_policy_without_overwriting_builtin_or_user_policy() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), Zeroizing::new([3; 32])).unwrap();
        let mut rule = Rule {
            id: Uuid::new_v4(),
            name: "测试".into(),
            entity_type: "PERSON".into(),
            kind: domain::RuleKind::Literal,
            pattern: "张三".into(),
            enabled: true,
        };
        store.save_rule(&rule).unwrap();
        assert!(store.policies().unwrap().is_empty());
        rule.entity_type = "内部项目".into();
        store.save_rule(&rule).unwrap();
        assert_eq!(store.policies().unwrap()[0].replacement, "已脱敏");
        store
            .save_policy(&Policy {
                entity_type: "内部项目".into(),
                replacement: "某项目".into(),
            })
            .unwrap();
        store.save_rule(&rule).unwrap();
        assert_eq!(store.policies().unwrap()[0].replacement, "某项目");
    }
    #[test]
    fn payload_is_encrypted_and_bound_to_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Zeroizing::new([7; 32])).unwrap();
        let id = Uuid::new_v4();
        store.save_payload(id, &"张三 13812345678").unwrap();
        let data = std::fs::read(store.task_dir(id).join("analysis.enc")).unwrap();
        assert!(!data.windows(11).any(|w| w == b"13812345678"));
        assert_eq!(store.payload::<String>(id).unwrap(), "张三 13812345678");
        let other = Uuid::new_v4();
        std::fs::create_dir_all(store.task_dir(other)).unwrap();
        std::fs::write(store.task_dir(other).join("analysis.enc"), data).unwrap();
        assert!(store.payload::<String>(other).is_err());
    }

    #[test]
    fn sidecars_are_encrypted_and_bound_to_their_file_kind() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Zeroizing::new([14; 32])).unwrap();
        let id = Uuid::new_v4();
        store.save_review(id, &"复核内容").unwrap();
        store.save_output(id, b"generated output").unwrap();
        assert_eq!(
            store.review::<String>(id).unwrap().as_deref(),
            Some("复核内容")
        );
        assert_eq!(
            store.output(id).unwrap().as_deref(),
            Some(b"generated output".as_slice())
        );
        let review = std::fs::read(store.task_dir(id).join("review.enc")).unwrap();
        std::fs::write(store.task_dir(id).join("output.enc"), review).unwrap();
        assert!(store.output(id).is_err());
    }

    #[test]
    fn concurrent_workers_wait_for_sqlite_writes() {
        let dir = tempfile::tempdir().unwrap();
        let workers = (0..4)
            .map(|_| Store::open(dir.path(), Zeroizing::new([11; 32])).unwrap())
            .collect::<Vec<_>>();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(workers.len()));
        std::thread::scope(|scope| {
            for (worker_index, store) in workers.into_iter().enumerate() {
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    for item in 0..25 {
                        store
                            .save_meta(&TaskMeta {
                                id: Uuid::new_v4(),
                                kind: format!("worker-{worker_index}-{item}"),
                                state: domain::TaskState::Completed,
                                created_at: item,
                                updated_at: item,
                                error: None,
                                error_info: None,
                                display_name: format!("任务 {item}"),
                                file_size: 0,
                                parent_batch_id: None,
                                reviewed_revision: 0,
                                storage_bytes: 0,
                            })
                            .unwrap();
                    }
                });
            }
        });
        let store = Store::open(dir.path(), Zeroizing::new([11; 32])).unwrap();
        assert_eq!(store.tasks().unwrap().len(), 100);
    }

    #[test]
    fn old_task_json_uses_compatible_defaults() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"id":"{id}","kind":"txt","state":"completed","created_at":1,"updated_at":2,"error":null}}"#
        );
        let meta: TaskMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta.display_name, "");
        assert_eq!(meta.file_size, 0);
        assert_eq!(meta.parent_batch_id, None);
        assert_eq!(meta.reviewed_revision, 0);
        assert_eq!(meta.storage_bytes, 0);
        assert_eq!(meta.error_info, None);
    }

    #[test]
    fn corrupt_task_record_is_quarantined_without_hiding_valid_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Zeroizing::new([12; 32])).unwrap();
        let valid = TaskMeta {
            id: Uuid::new_v4(),
            kind: "txt".into(),
            state: domain::TaskState::Completed,
            created_at: 1,
            updated_at: 2,
            error: None,
            error_info: None,
            display_name: "输入.txt".into(),
            file_size: 12,
            parent_batch_id: None,
            reviewed_revision: 1,
            storage_bytes: 0,
        };
        store.save_meta(&valid).unwrap();
        store
            .db
            .execute(
                "INSERT INTO tasks(id, metadata) VALUES(?1, ?2)",
                params![Uuid::new_v4().to_string(), "{broken"],
            )
            .unwrap();

        let tasks = store.tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, valid.id);
        let quarantined: i64 = store
            .db
            .query_row("SELECT COUNT(*) FROM corrupt_tasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(quarantined, 1);
    }

    #[test]
    fn future_database_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("db")).unwrap();
        let db = Connection::open(dir.path().join("db/app.sqlite3")).unwrap();
        db.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(db);
        assert!(Store::open(dir.path(), Zeroizing::new([13; 32])).is_err());
    }
}
