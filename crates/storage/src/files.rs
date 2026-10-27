use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputRecord {
    pub id: Uuid,
    pub task_id: Uuid,
    pub name: String,
    pub kind: String,
    pub path: String,
    pub created_at: u64,
    pub size: u64,
    pub error: Option<String>,
    pub generation: String,
    pub partial: bool,
    #[serde(default)]
    pub failed_count: usize,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub exists: bool,
    #[serde(default)]
    pub task_exists: bool,
}

#[derive(Default, Deserialize)]
pub struct OutputQuery {
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub kind: String,
    pub task_id: Option<Uuid>,
    pub directory: Option<String>,
    #[serde(default)]
    pub offset: u32,
}

#[derive(Serialize)]
pub struct OutputPage {
    pub items: Vec<OutputRecord>,
    pub total: u64,
}

impl Store {
    pub fn output_state(&self, id: Uuid) -> Result<&'static str> {
        let mut stmt = self.db.prepare("SELECT json_extract(data,'$.path'), json_extract(data,'$.error') FROM outputs WHERE task_id=?1").map_err(db_err)?;
        let rows = stmt
            .query_map([id.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(db_err)?;
        let mut saved = false;
        for row in rows {
            let (path, error) = row.map_err(db_err)?;
            if error.is_none() {
                saved = true;
                if Path::new(&path).is_file() {
                    return Ok("saved");
                }
            }
        }
        Ok(if saved { "missing" } else { "unsaved" })
    }
    pub fn output_directory(&self) -> Result<Option<PathBuf>> {
        Ok(self
            .config::<PathBuf>("output_directory")?
            .into_iter()
            .next())
    }

    pub fn save_output_directory(&self, path: &Path) -> Result<()> {
        if !path.is_absolute() || !path.is_dir() {
            return Err(Error::Invalid("请选择已存在的绝对目录".into()));
        }
        tempfile::NamedTempFile::new_in(path)?;
        self.put("output_directory", "app", &path)
    }

    pub fn put_output(&self, record: &OutputRecord, auto_key: Option<&str>) -> Result<()> {
        self.db
            .execute(
                "INSERT INTO outputs(id,task_id,auto_key,data) VALUES(?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET data=excluded.data",
                params![
                    record.id.to_string(),
                    record.task_id.to_string(),
                    auto_key,
                    serde_json::to_string(record).map_err(json_err)?
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn output_record(&self, id: Uuid) -> Result<OutputRecord> {
        let data: String = self
            .db
            .query_row(
                "SELECT data FROM outputs WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| Error::Invalid("文件记录不存在".into()))?;
        serde_json::from_str(&data).map_err(json_err)
    }

    pub fn automatic_output(&self, key: &str) -> Result<Option<OutputRecord>> {
        let data: Option<String> = self
            .db
            .query_row("SELECT data FROM outputs WHERE auto_key=?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(db_err)?;
        data.map(|data| serde_json::from_str(&data).map_err(json_err))
            .transpose()
    }

    pub fn output_page(&self, query: OutputQuery) -> Result<OutputPage> {
        // instr treats %, _ and user text literally; only the visible page is stat'ed.
        let filter = "WHERE (?1='' OR instr(lower(json_extract(data,'$.name')),lower(?1))>0) AND (?2='' OR json_extract(data,'$.kind')=?2) AND (?3='' OR task_id=?3) AND (?5='' OR replace(replace(substr(json_extract(data,'$.path'),1,length(json_extract(data,'$.path'))-length(json_extract(data,'$.name'))),char(92),'/'),'//?/','')=?5)";
        let directory = query
            .directory
            .map(|p| {
                format!(
                    "{}/",
                    p.replace('\\', "/")
                        .trim_start_matches("//?/")
                        .trim_end_matches('/')
                )
            })
            .unwrap_or_default();
        let task = query.task_id.map(|id| id.to_string()).unwrap_or_default();
        let total: i64 = self
            .db
            .query_row(
                &format!("SELECT count(*) FROM outputs {filter}"),
                params![query.search, query.kind, task, query.offset, directory],
                |r| r.get(0),
            )
            .map_err(db_err)?;
        let mut stmt = self.db.prepare(&format!("SELECT data, EXISTS(SELECT 1 FROM tasks WHERE tasks.id=outputs.task_id) FROM outputs {filter} ORDER BY json_extract(data,'$.created_at') DESC,id DESC LIMIT 30 OFFSET ?4")).map_err(db_err)?;
        let rows = stmt
            .query_map(
                params![query.search, query.kind, task, query.offset, directory],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
            )
            .map_err(db_err)?;
        let mut items = Vec::new();
        for row in rows {
            let (data, task_exists) = row.map_err(db_err)?;
            let mut record: OutputRecord = serde_json::from_str(&data).map_err(json_err)?;
            record.exists = record.error.is_none() && Path::new(&record.path).is_file();
            record.task_exists = task_exists;
            items.push(record);
        }
        Ok(OutputPage {
            items,
            total: total as u64,
        })
    }
}
