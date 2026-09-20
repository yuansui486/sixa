use domain::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

pub const MODEL_REPOSITORY: &str = "yuansui486/data_desensitization_0918";
pub const MODEL_REVISION: &str = "raner-v1.0.0";
pub const DESKTOP_MODEL_REVISION: &str = "desktop-models-v1.0.0";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    pub source: String,
    pub license: String,
    pub files: Vec<ModelFile>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelStatus {
    pub ready: bool,
    pub version: Option<String>,
    pub location: String,
    pub bytes: u64,
    pub error: Option<String>,
}
#[derive(Clone)]
pub struct VerifiedModel {
    directory: PathBuf,
    manifest: Manifest,
}
impl VerifiedModel {
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn require(&self, names: &[&str]) -> Result<()> {
        for name in names {
            if !self
                .manifest
                .files
                .iter()
                .any(|file| file.name.eq_ignore_ascii_case(name))
            {
                return Err(Error::ModelsNotReady(format!("清单缺少 {name}")));
            }
        }
        Ok(())
    }
}
pub fn verify_model(dir: &Path) -> Result<VerifiedModel> {
    verify_model_with_required(
        dir,
        &[
            "emissions.onnx",
            "tokenizer.json",
            "crf.json",
            "onnxruntime.dll",
        ],
    )
}
pub fn verify_model_with_required(dir: &Path, required: &[&str]) -> Result<VerifiedModel> {
    Ok(VerifiedModel {
        directory: dir.to_path_buf(),
        manifest: verify_with_required(dir, required)?,
    })
}
pub fn inspect_model(dir: &Path) -> Result<VerifiedModel> {
    inspect_model_with_required(
        dir,
        &[
            "emissions.onnx",
            "tokenizer.json",
            "crf.json",
            "onnxruntime.dll",
        ],
    )
}
pub fn inspect_model_with_required(dir: &Path, required: &[&str]) -> Result<VerifiedModel> {
    Ok(VerifiedModel {
        directory: dir.to_path_buf(),
        manifest: inspect_with_required(dir, required)?,
    })
}
pub fn verify(dir: &Path) -> Result<Manifest> {
    verify_with_required(
        dir,
        &[
            "emissions.onnx",
            "tokenizer.json",
            "crf.json",
            "onnxruntime.dll",
        ],
    )
}

pub fn verify_with_required(dir: &Path, required: &[&str]) -> Result<Manifest> {
    check_with_required(dir, required, true)
}
pub fn inspect_with_required(dir: &Path, required: &[&str]) -> Result<Manifest> {
    check_with_required(dir, required, false)
}
fn check_with_required(dir: &Path, required: &[&str], hash_files: bool) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json"))
            .map_err(|_| Error::ModelsNotReady("未安装 RaNER ONNX 模型包".into()))?,
    )
    .map_err(|e| Error::ModelsNotReady(e.to_string()))?;
    if manifest.schema != 1 || manifest.version.is_empty() || manifest.files.is_empty() {
        return Err(Error::ModelsNotReady("模型清单版本无效".into()));
    }
    let mut names = std::collections::HashSet::new();
    let mut total = 0u64;
    for file in &manifest.files {
        if file.name.is_empty()
            || file.name.contains(['/', '\\', ':'])
            || file.name == "."
            || file.name == ".."
            || file.name.ends_with(['.', ' '])
            || !names.insert(file.name.to_lowercase())
        {
            return Err(Error::ModelsNotReady("模型清单路径无效或重复".into()));
        }
        total = total
            .checked_add(file.size)
            .ok_or_else(|| Error::ModelsNotReady("模型包大小溢出".into()))?;
        if total > 2 * 1024 * 1024 * 1024 {
            return Err(Error::ModelsNotReady("模型包超过 2 GB 限制".into()));
        }
        let path = dir.join(&file.name);
        let mut input = std::fs::File::open(&path)
            .map_err(|_| Error::ModelsNotReady(format!("缺少 {}", file.name)))?;
        if !input.metadata()?.is_file() {
            return Err(Error::ModelsNotReady(format!("{} 不是文件", file.name)));
        }
        if hash_files {
            if input.metadata()?.len() != file.size {
                return Err(Error::ModelsNotReady(format!("{} 大小不符", file.name)));
            }
            let mut hash = Sha256::new();
            let mut buffer = [0; 64 * 1024];
            loop {
                let n = input.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            if format!("{:x}", hash.finalize()) != file.sha256.to_lowercase() {
                return Err(Error::ModelsNotReady(format!(
                    "{} SHA-256 校验失败",
                    file.name
                )));
            }
        }
    }
    for required in required {
        if !names.contains(&required.to_lowercase()) {
            return Err(Error::ModelsNotReady(format!("清单缺少 {required}")));
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_rejects_corruption_and_windows_case_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"synthetic model fixture";
        let files: Vec<_> = [
            "emissions.onnx",
            "tokenizer.json",
            "crf.json",
            "onnxruntime.dll",
        ]
        .into_iter()
        .map(|name| {
            std::fs::write(dir.path().join(name), content).unwrap();
            ModelFile {
                name: name.into(),
                size: content.len() as u64,
                sha256: format!("{:x}", Sha256::digest(content)),
            }
        })
        .collect();
        let mut manifest = Manifest {
            schema: 1,
            version: "test".into(),
            source: "test".into(),
            license: "test".into(),
            files,
        };
        let save = |m: &Manifest| {
            std::fs::write(
                dir.path().join("manifest.json"),
                serde_json::to_vec(m).unwrap(),
            )
            .unwrap()
        };
        save(&manifest);
        assert!(verify(dir.path()).is_ok());
        assert!(inspect_model(dir.path()).is_ok());
        assert!(verify_with_required(dir.path(), &["CRF.JSON"]).is_ok());
        std::fs::write(dir.path().join("crf.json"), vec![0; content.len()]).unwrap();
        assert!(verify(dir.path()).is_err());
        assert!(inspect_model(dir.path()).is_ok());
        std::fs::remove_file(dir.path().join("crf.json")).unwrap();
        assert!(inspect_model(dir.path()).is_err());
        std::fs::write(dir.path().join("crf.json"), content).unwrap();
        manifest.files.push(ModelFile {
            name: "CRF.JSON".into(),
            size: content.len() as u64,
            sha256: format!("{:x}", Sha256::digest(content)),
        });
        save(&manifest);
        assert!(verify(dir.path()).is_err());
    }
}
