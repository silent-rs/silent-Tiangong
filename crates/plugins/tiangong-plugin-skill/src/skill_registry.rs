use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::skill_config::{SkillMcpRequirementConfig, SkillPermissionConfig, SkillSourceConfig};

pub const DEFAULT_SKILL_REGISTRY_CACHE_TTL: Duration = Duration::from_secs(2);
pub const DEFAULT_SKILL_REGISTRY_LOADED_CAPACITY: usize = 32;

#[derive(Debug, Clone)]
pub struct SkillRegistryEntry {
    pub id: String,
    pub dir: PathBuf,
    pub manifest_mtime: SystemTime,
}

#[derive(Debug, Clone)]
pub struct SkillRegistryView {
    pub entries: HashMap<String, SkillRegistryEntry>,
    pub issues: Vec<SkillRegistryIssue>,
    pub scanned_at: Instant,
}

#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub manifest: SkillManifest,
    pub readme: String,
    pub loaded_at: Instant,
    pub source_mtime: SystemTime,
}

#[derive(Debug, Clone)]
pub struct SkillRegistryIssue {
    pub kind: SkillRegistryIssueKind,
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRegistryIssueKind {
    MissingManifest,
    InvalidManifest,
    IdMismatch,
    MissingEntry,
    DuplicateId,
    Inaccessible,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default = "default_available")]
    pub available: bool,
    #[serde(default)]
    pub source: SkillSourceConfig,
    #[serde(default)]
    pub requires: SkillManifestRequires,
    #[serde(default)]
    pub permissions: SkillPermissionConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillManifestRequires {
    #[serde(default)]
    pub mcp: Vec<SkillMcpRequirementConfig>,
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Debug)]
pub struct SkillRegistry {
    root: PathBuf,
    cache_ttl: Duration,
    loaded_capacity: usize,
    view: RwLock<Option<SkillRegistryView>>,
    loaded: RwLock<HashMap<String, Arc<LoadedSkill>>>,
}

impl SkillRegistry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache_ttl: DEFAULT_SKILL_REGISTRY_CACHE_TTL,
            loaded_capacity: DEFAULT_SKILL_REGISTRY_LOADED_CAPACITY,
            view: RwLock::new(None),
            loaded: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_options(
        root: impl Into<PathBuf>,
        cache_ttl: Duration,
        loaded_capacity: usize,
    ) -> Self {
        Self {
            root: root.into(),
            cache_ttl,
            loaded_capacity: loaded_capacity.max(1),
            view: RwLock::new(None),
            loaded: RwLock::new(HashMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn refresh(&self) -> SkillRegistryView {
        let view = scan_skill_registry(&self.root);
        let mut guard = self.view.write().unwrap_or_else(|err| err.into_inner());
        *guard = Some(view.clone());
        view
    }

    pub fn view(&self) -> SkillRegistryView {
        let now = Instant::now();
        {
            let guard = self.view.read().unwrap_or_else(|err| err.into_inner());
            if let Some(view) = guard.as_ref()
                && now.duration_since(view.scanned_at) <= self.cache_ttl
            {
                return view.clone();
            }
        }
        self.refresh()
    }

    pub fn get(&self, id: &str) -> Result<Arc<LoadedSkill>> {
        let id = id.trim();
        if id.is_empty() {
            return Err(anyhow!("skill id 不能为空"));
        }

        let view = self.view();
        let entry = view
            .entries
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("未找到 skill：{id}"))?;

        {
            let loaded = self.loaded.read().unwrap_or_else(|err| err.into_inner());
            if let Some(skill) = loaded.get(id)
                && skill.source_mtime == entry.manifest_mtime
            {
                return Ok(Arc::clone(skill));
            }
        }

        let skill = Arc::new(load_skill_entry(&entry)?);
        let mut loaded = self.loaded.write().unwrap_or_else(|err| err.into_inner());
        loaded.insert(id.to_string(), Arc::clone(&skill));
        evict_loaded_cache(&mut loaded, self.loaded_capacity);
        Ok(skill)
    }

    pub fn is_available(&self, id: &str) -> Result<bool> {
        Ok(self.get(id)?.manifest.available)
    }

    pub fn set_available(&self, id: &str, available: bool) -> Result<()> {
        let id = id.trim();
        if id.is_empty() {
            return Err(anyhow!("skill id 不能为空"));
        }
        let view = self.refresh();
        let entry = view
            .entries
            .get(id)
            .ok_or_else(|| anyhow!("未找到 skill：{id}"))?;
        write_skill_available(&entry.dir, available)?;
        self.invalidate(id);
        self.refresh();
        Ok(())
    }

    pub fn invalidate(&self, id: &str) {
        let mut loaded = self.loaded.write().unwrap_or_else(|err| err.into_inner());
        loaded.remove(id);
    }
}

pub fn scan_skill_registry(root: &Path) -> SkillRegistryView {
    let mut entries = HashMap::new();
    let mut issues = Vec::new();

    let Ok(read_dir) = fs::read_dir(root) else {
        if root.exists() {
            issues.push(issue(
                SkillRegistryIssueKind::Inaccessible,
                root,
                "无法读取 skills 目录",
            ));
        }
        return SkillRegistryView {
            entries,
            issues,
            scanned_at: Instant::now(),
        };
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(dir_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if dir_name.starts_with('.') || dir_name == "mcp-lock.json" || !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("skill.toml");
        let Ok(metadata) = fs::metadata(&manifest_path) else {
            issues.push(issue(
                SkillRegistryIssueKind::MissingManifest,
                &path,
                "缺少 skill.toml",
            ));
            continue;
        };
        let manifest_mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let manifest = match read_skill_manifest(&manifest_path) {
            Ok(manifest) => manifest,
            Err(err) => {
                issues.push(issue(
                    SkillRegistryIssueKind::InvalidManifest,
                    &manifest_path,
                    &format!("解析 skill.toml 失败：{err}"),
                ));
                continue;
            }
        };
        if manifest.id.trim() != dir_name {
            issues.push(issue(
                SkillRegistryIssueKind::IdMismatch,
                &path,
                &format!("目录名 {dir_name} 与 skill.toml.id {} 不一致", manifest.id),
            ));
            continue;
        }
        if entries.contains_key(&manifest.id) {
            issues.push(issue(
                SkillRegistryIssueKind::DuplicateId,
                &path,
                &format!("重复 skill id：{}", manifest.id),
            ));
            continue;
        }
        entries.insert(
            manifest.id.clone(),
            SkillRegistryEntry {
                id: manifest.id,
                dir: path,
                manifest_mtime,
            },
        );
    }

    SkillRegistryView {
        entries,
        issues,
        scanned_at: Instant::now(),
    }
}

pub fn read_skill_manifest(path: &Path) -> Result<SkillManifest> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("读取 skill.toml 失败：{}", path.display()))?;
    let mut manifest: SkillManifest = toml::from_str(&raw)
        .with_context(|| format!("解析 skill.toml 失败：{}", path.display()))?;
    manifest.id = manifest.id.trim().to_string();
    manifest.name = manifest.name.trim().to_string();
    manifest.version = manifest.version.trim().to_string();
    manifest.entry = manifest.entry.trim().to_string();
    if manifest.entry.is_empty() {
        manifest.entry = default_entry();
    }
    Ok(manifest)
}

pub fn write_skill_available(skill_dir: &Path, available: bool) -> Result<()> {
    let manifest_path = skill_dir.join("skill.toml");
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("读取 skill.toml 失败：{}", manifest_path.display()))?;
    let mut value: toml::Value = toml::from_str(&raw)
        .with_context(|| format!("解析 skill.toml 失败：{}", manifest_path.display()))?;
    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow!("skill.toml 根节点必须是 table：{}", manifest_path.display()))?;
    table.insert("available".to_string(), toml::Value::Boolean(available));
    let content = toml::to_string_pretty(&value)
        .with_context(|| format!("序列化 skill.toml 失败：{}", manifest_path.display()))?;
    fs::write(&manifest_path, content)
        .with_context(|| format!("写入 skill.toml 失败：{}", manifest_path.display()))?;
    Ok(())
}

fn load_skill_entry(entry: &SkillRegistryEntry) -> Result<LoadedSkill> {
    let manifest_path = entry.dir.join("skill.toml");
    let manifest = read_skill_manifest(&manifest_path)?;
    if !manifest.available {
        return Ok(LoadedSkill {
            manifest,
            readme: String::new(),
            loaded_at: Instant::now(),
            source_mtime: entry.manifest_mtime,
        });
    }

    let readme_path = entry.dir.join(&manifest.entry);
    let readme = fs::read_to_string(&readme_path)
        .with_context(|| format!("读取 Skill 入口失败：{}", readme_path.display()))?;
    Ok(LoadedSkill {
        manifest,
        readme,
        loaded_at: Instant::now(),
        source_mtime: entry.manifest_mtime,
    })
}

fn evict_loaded_cache(cache: &mut HashMap<String, Arc<LoadedSkill>>, capacity: usize) {
    while cache.len() > capacity {
        let Some(oldest_id) = cache
            .iter()
            .min_by_key(|(_, skill)| skill.loaded_at)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        cache.remove(&oldest_id);
    }
}

fn issue(kind: SkillRegistryIssueKind, path: &Path, message: &str) -> SkillRegistryIssue {
    SkillRegistryIssue {
        kind,
        path: path.to_path_buf(),
        message: message.to_string(),
    }
}

fn default_available() -> bool {
    true
}

fn default_entry() -> String {
    "SKILL.md".to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    fn write_skill(root: &Path, id: &str, available: Option<bool>, body: &str) -> PathBuf {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        let available_line = available
            .map(|value| format!("available = {value}\n"))
            .unwrap_or_default();
        fs::write(
            dir.join("skill.toml"),
            format!(
                "id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\n{available_line}"
            ),
        )
        .unwrap();
        fs::write(dir.join("SKILL.md"), body).unwrap();
        dir
    }

    #[test]
    fn scan_reads_flat_skill_directory_without_loading_readme() {
        let temp = TempDir::new().unwrap();
        let skill_dir = write_skill(temp.path(), "demo-skill", None, "# Demo\nbody");

        let view = scan_skill_registry(temp.path());

        assert_eq!(view.entries.len(), 1);
        let entry = view.entries.get("demo-skill").unwrap();
        assert_eq!(entry.dir, skill_dir);
        assert!(view.issues.is_empty());
    }

    #[test]
    fn scan_skips_directory_name_manifest_id_mismatch() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("dir-id");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("skill.toml"),
            "id = \"manifest-id\"\nname = \"Mismatch\"\n",
        )
        .unwrap();

        let view = scan_skill_registry(temp.path());

        assert!(view.entries.is_empty());
        assert_eq!(view.issues.len(), 1);
        assert_eq!(view.issues[0].kind, SkillRegistryIssueKind::IdMismatch);
    }

    #[test]
    fn manifest_available_defaults_to_true() {
        let temp = TempDir::new().unwrap();
        let dir = write_skill(temp.path(), "available-default", None, "# Demo\nbody");

        let manifest = read_skill_manifest(&dir.join("skill.toml")).unwrap();

        assert!(manifest.available);
    }

    #[test]
    fn unavailable_skill_does_not_load_readme() {
        let temp = TempDir::new().unwrap();
        write_skill(
            temp.path(),
            "disabled-skill",
            Some(false),
            "# Disabled\nshould not be loaded",
        );
        let registry = SkillRegistry::new(temp.path());

        let skill = registry.get("disabled-skill").unwrap();

        assert!(!skill.manifest.available);
        assert!(skill.readme.is_empty());
    }

    #[test]
    fn set_available_updates_manifest_file_and_invalidates_cache() {
        let temp = TempDir::new().unwrap();
        write_skill(temp.path(), "toggle-skill", None, "# Toggle\nbody");
        let registry = SkillRegistry::new(temp.path());

        assert!(registry.is_available("toggle-skill").unwrap());
        registry.set_available("toggle-skill", false).unwrap();

        let manifest = read_skill_manifest(&temp.path().join("toggle-skill/skill.toml")).unwrap();
        assert!(!manifest.available);
        let loaded = registry.get("toggle-skill").unwrap();
        assert!(!loaded.manifest.available);
        assert!(loaded.readme.is_empty());
    }

    #[test]
    fn get_reloads_when_manifest_mtime_changes_after_refresh() {
        let temp = TempDir::new().unwrap();
        let dir = write_skill(temp.path(), "reload-skill", None, "# Demo\nfirst");
        let registry = SkillRegistry::with_options(temp.path(), Duration::from_secs(60), 32);

        let first = registry.get("reload-skill").unwrap();
        assert!(first.readme.contains("first"));

        std::thread::sleep(Duration::from_millis(5));
        fs::write(
            dir.join("skill.toml"),
            "id = \"reload-skill\"\nname = \"reload-skill\"\nversion = \"0.2.0\"\nentry = \"SKILL.md\"\n",
        )
        .unwrap();
        fs::write(dir.join("SKILL.md"), "# Demo\nsecond").unwrap();
        registry.refresh();

        let second = registry.get("reload-skill").unwrap();
        assert_eq!(second.manifest.version, "0.2.0");
        assert!(second.readme.contains("second"));
    }

    #[test]
    fn refresh_bypasses_view_cache() {
        let temp = TempDir::new().unwrap();
        let registry = SkillRegistry::with_options(temp.path(), Duration::from_secs(60), 32);

        assert!(registry.view().entries.is_empty());

        write_skill(temp.path(), "manual-skill", None, "# Manual\nbody");
        assert!(
            registry.view().entries.is_empty(),
            "ttl 内普通 view 应复用旧扫描结果"
        );

        let refreshed = registry.refresh();
        assert!(refreshed.entries.contains_key("manual-skill"));
    }
}
