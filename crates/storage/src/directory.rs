use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize)]
pub struct DirectoryQuery {
    #[serde(default)]
    pub relative: String,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub offset: usize,
}
#[derive(Serialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
    pub directory: bool,
    pub size: u64,
    pub modified: u64,
    pub output: Option<files::OutputRecord>,
}
#[derive(Serialize)]
pub struct DirectoryPage {
    pub root: PathBuf,
    pub path: PathBuf,
    pub relative: PathBuf,
    pub items: Vec<DirectoryEntry>,
    pub total: usize,
}
pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf> {
    if Path::new(relative).components().any(|c| {
        !matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return Err(Error::Invalid("请选择保存根目录内的文件夹".into()));
    }
    let root = root.canonicalize()?;
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(&root) {
        return Err(Error::Invalid("目录链接指向保存根目录之外".into()));
    }
    Ok(path)
}
pub fn valid_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return Err(Error::Invalid("名称不能包含路径分隔符或特殊字符".into()));
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(Error::Invalid("请使用非系统保留名称".into()));
    }
    Ok(())
}
pub fn create(root: &Path, relative: &str, name: &str) -> Result<PathBuf> {
    valid_name(name)?;
    let path = resolve(root, relative)?.join(name);
    std::fs::create_dir(&path)?;
    Ok(path)
}
impl Store {
    pub fn directory_page(&self, root: &Path, query: DirectoryQuery) -> Result<DirectoryPage> {
        let root = root.canonicalize()?;
        let path = resolve(&root, &query.relative)?;
        let mut entries = Vec::new();
        for item in std::fs::read_dir(&path)? {
            let item = item?;
            let name = item.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !name.to_lowercase().contains(&query.search.to_lowercase())
            {
                continue;
            }
            let Ok(actual) = item.path().canonicalize() else {
                continue;
            };
            if !actual.starts_with(&root) {
                continue;
            }
            let metadata = item.metadata()?;
            if !metadata.is_dir() && !metadata.is_file() {
                continue;
            }
            if !metadata.is_dir()
                && !query.kind.is_empty()
                && item
                    .path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase()
                    != query.kind
            {
                continue;
            }
            entries.push(DirectoryEntry {
                name,
                path: item.path(),
                directory: metadata.is_dir(),
                size: metadata.len(),
                modified: metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|t| t.as_secs())
                    .unwrap_or(0),
                output: None,
            });
        }
        entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        let total = entries.len();
        let mut items: Vec<_> = entries.into_iter().skip(query.offset).take(30).collect();
        let mut stmt = self.db.prepare("SELECT data, EXISTS(SELECT 1 FROM tasks WHERE tasks.id=outputs.task_id) FROM outputs WHERE json_extract(data,'$.name')=?1 COLLATE NOCASE ORDER BY json_extract(data,'$.created_at') DESC").map_err(db_err)?;
        for item in items.iter_mut().filter(|i| !i.directory) {
            let rows = stmt
                .query_map([&item.name], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
                })
                .map_err(db_err)?;
            let actual = item.path.canonicalize()?;
            for row in rows {
                let (data, task_exists) = row.map_err(db_err)?;
                let mut output: files::OutputRecord =
                    serde_json::from_str(&data).map_err(json_err)?;
                if output.error.is_some()
                    || Path::new(&output.path).canonicalize().ok().as_ref() != Some(&actual)
                {
                    continue;
                }
                output.exists = true;
                output.task_exists = task_exists;
                item.output = Some(output);
                break;
            }
        }
        Ok(DirectoryPage {
            relative: path.strip_prefix(&root).unwrap_or(Path::new("")).into(),
            root,
            path,
            items,
            total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn external_symlinks_are_not_visible_or_writable() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data"), Zeroizing::new([1; 32])).unwrap();
        let root = temp.path().join("results");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        assert_eq!(
            store
                .directory_page(&root, DirectoryQuery::default())
                .unwrap()
                .total,
            0
        );
        assert!(resolve(&root, "escape").is_err());
        assert!(create(&root, "escape", "nested").is_err());
        assert!(!outside.join("nested").exists());
    }
    #[test]
    fn browsing_is_scoped_to_actual_directory() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data"), Zeroizing::new([1; 32])).unwrap();
        let root = temp.path().join("results");
        std::fs::create_dir(&root).unwrap();
        create(&root, "", "项目").unwrap();
        std::fs::write(root.join("外部文件.txt"), "text").unwrap();
        std::fs::write(root.join("项目/内部.txt"), "text").unwrap();
        let page = store
            .directory_page(&root, DirectoryQuery::default())
            .unwrap();
        assert_eq!(page.total, 2);
        assert!(page.items[0].directory);
        assert!(page.items[1].output.is_none());
        assert!(resolve(&root, "../data").is_err());
        assert!(create(&root, "", "../escape").is_err());
        assert!(create(&root, "", "CON.txt").is_err());
        std::fs::remove_file(root.join("外部文件.txt")).unwrap();
        assert_eq!(
            store
                .directory_page(&root, DirectoryQuery::default())
                .unwrap()
                .total,
            1
        );
    }
}
