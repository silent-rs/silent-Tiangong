//! Skill 注册表核心（从原生插件 `skill_registry.rs` 迁入）。
//!
//! 扫描 `~/.tiangong/skills/`，读每个 skill 的 `skill.toml` 和 `SKILL.md`，
//! 维护内存缓存（view + loaded）。所有文件系统操作集中在此。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow};

use tiangong_plugin_skill_protocol::SkillManifest;

pub const DEFAULT_SKILL_REGISTRY_CACHE_TTL: Duration = Duration::from_secs(2);
pub const DEFAULT_SKILL_REGISTRY_LOADED_CAPACITY: usize = 32;

#[derive(Debug, Clone)]
pub struct SkillRegistryEntry {
    pub id: String,
    pub dir: PathBuf,
    pub manifest_mtime: SystemTime,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
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
#[allow(dead_code)]
pub struct SkillRegistryIssue {
    pub kind: SkillRegistryIssueKind,
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SkillRegistryIssueKind {
    MissingManifest,
    InvalidManifest,
    IdMismatch,
    MissingEntry,
    DuplicateId,
    Inaccessible,
}

/// Skill 注册表：扫描目录 + 缓存 view/loaded。
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

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 强制重扫目录（绕过 TTL 缓存）。
    pub fn refresh(&self) -> SkillRegistryView {
        let view = scan_skill_registry(&self.root);
        let mut guard = self.view.write().unwrap_or_else(|err| err.into_inner());
        *guard = Some(view.clone());
        view
    }

    /// 取注册表视图（命中 TTL 缓存则直接返回，否则重扫）。
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

    /// 按 id 加载完整 skill（含 SKILL.md 全文），带 mtime 缓存。
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
        {
            let mut loaded = self.loaded.write().unwrap_or_else(|err| err.into_inner());
            loaded.insert(id.to_string(), Arc::clone(&skill));
            evict_loaded_cache(&mut loaded, self.loaded_capacity);
        }
        Ok(skill)
    }

    /// 启用/禁用 skill：写 skill.toml 的 available 字段。
    pub fn set_available(&self, id: &str, available: bool) -> Result<()> {
        let id = id.trim();
        if id.is_empty() {
            return Err(anyhow!("skill id 不能为空"));
        }
        self.invalidate(id);
        let view = self.refresh();
        let entry = view
            .entries
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("未找到 skill：{id}"))?;
        write_skill_available(&entry.dir, available)
            .with_context(|| format!("设置 skill available 失败：{id}"))?;
        self.invalidate(id);
        self.refresh();
        Ok(())
    }

    /// 失效指定 id 的 loaded 缓存。
    pub fn invalidate(&self, id: &str) {
        let mut loaded = self.loaded.write().unwrap_or_else(|err| err.into_inner());
        loaded.remove(id);
    }
}

/// 扫描 skills 目录，构建注册表视图。
pub fn scan_skill_registry(root: &Path) -> SkillRegistryView {
    let mut entries = HashMap::new();
    let mut issues = Vec::new();

    let read_dir = match fs::read_dir(root) {
        Ok(rd) => rd,
        Err(_) => {
            // 目录不存在或不可读：若 root 存在则记录，否则视为空（首次使用）。
            if root.exists() {
                issues.push(issue(
                    SkillRegistryIssueKind::Inaccessible,
                    root.to_path_buf(),
                    "无法读取 skills 目录",
                ));
            }
            return SkillRegistryView {
                entries,
                issues,
                scanned_at: Instant::now(),
            };
        }
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // 跳过隐藏目录和 mcp-lock.json。
        if file_name.starts_with('.') || file_name == "mcp-lock.json" {
            continue;
        }
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("skill.toml");
        let metadata = match fs::metadata(&manifest_path) {
            Ok(m) => m,
            Err(_) => {
                issues.push(issue(
                    SkillRegistryIssueKind::MissingManifest,
                    manifest_path.clone(),
                    &format!("缺少 skill.toml：{file_name}"),
                ));
                continue;
            }
        };
        let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        let manifest = match read_skill_manifest(&manifest_path) {
            Ok(m) => m,
            Err(e) => {
                issues.push(issue(
                    SkillRegistryIssueKind::InvalidManifest,
                    manifest_path.clone(),
                    &format!("skill.toml 解析失败：{e}"),
                ));
                continue;
            }
        };

        if manifest.id.trim() != file_name {
            issues.push(issue(
                SkillRegistryIssueKind::IdMismatch,
                manifest_path.clone(),
                &format!("skill id（{}）与目录名（{file_name}）不一致", manifest.id),
            ));
            continue;
        }

        if entries.contains_key(&manifest.id) {
            issues.push(issue(
                SkillRegistryIssueKind::DuplicateId,
                manifest_path.clone(),
                &format!("重复的 skill id：{}", manifest.id),
            ));
            continue;
        }

        entries.insert(
            manifest.id.clone(),
            SkillRegistryEntry {
                id: manifest.id.clone(),
                dir: path.clone(),
                manifest_mtime: mtime,
            },
        );
    }

    SkillRegistryView {
        entries,
        issues,
        scanned_at: Instant::now(),
    }
}

/// 读取并解析 skill.toml。
pub fn read_skill_manifest(path: &Path) -> Result<SkillManifest> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("读取 skill.toml 失败：{}", path.display()))?;
    let mut manifest: SkillManifest = toml::from_str(&content)
        .with_context(|| format!("解析 skill.toml 失败：{}", path.display()))?;
    manifest.id = manifest.id.trim().to_string();
    manifest.name = manifest.name.trim().to_string();
    manifest.version = manifest.version.trim().to_string();
    manifest.description = manifest.description.trim().to_string();
    manifest.entry = manifest.entry.trim().to_string();
    if manifest.entry.is_empty() {
        manifest.entry = "SKILL.md".to_string();
    }
    Ok(manifest)
}

/// 改写 skill.toml 的 available 字段（保留其他字段与注释）。
pub fn write_skill_available(skill_dir: &Path, available: bool) -> Result<()> {
    let manifest_path = skill_dir.join("skill.toml");
    let content = fs::read_to_string(&manifest_path)
        .with_context(|| format!("读取 skill.toml 失败：{}", manifest_path.display()))?;
    let mut value: toml::Value = toml::from_str(&content)
        .with_context(|| format!("解析 skill.toml 失败：{}", manifest_path.display()))?;
    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow!("skill.toml 不是合法的表：{}", manifest_path.display()))?;
    table.insert("available".to_string(), toml::Value::Boolean(available));
    let pretty = toml::to_string_pretty(&value)
        .with_context(|| format!("序列化 skill.toml 失败：{}", manifest_path.display()))?;
    fs::write(&manifest_path, pretty)
        .with_context(|| format!("写入 skill.toml 失败：{}", manifest_path.display()))?;
    Ok(())
}

/// 加载单个 skill 的完整内容（manifest + SKILL.md）。
fn load_skill_entry(entry: &SkillRegistryEntry) -> Result<LoadedSkill> {
    let manifest = read_skill_manifest(&entry.dir.join("skill.toml"))?;
    let readme = if !manifest.available {
        String::new()
    } else {
        let readme_path = entry.dir.join(&manifest.entry);
        fs::read_to_string(&readme_path).unwrap_or_default()
    };
    Ok(LoadedSkill {
        manifest,
        readme,
        loaded_at: Instant::now(),
        source_mtime: entry.manifest_mtime,
    })
}

/// LRU 式淘汰 loaded 缓存（按 loaded_at 最小者移除）。
fn evict_loaded_cache(cache: &mut HashMap<String, Arc<LoadedSkill>>, capacity: usize) {
    while cache.len() > capacity {
        let oldest = cache
            .iter()
            .min_by_key(|(_, skill)| skill.loaded_at)
            .map(|(id, _)| id.clone());
        match oldest {
            Some(id) => {
                cache.remove(&id);
            }
            None => break,
        }
    }
}

fn issue(kind: SkillRegistryIssueKind, path: PathBuf, message: &str) -> SkillRegistryIssue {
    SkillRegistryIssue {
        kind,
        path,
        message: message.to_string(),
    }
}
