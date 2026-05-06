//! 工作区文件树与 Rust 符号索引。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::types::{
    FileEntry, FileTreeIndex, FileType, SymbolEntry, SymbolIndex, SymbolKind, WorkspaceIndexHit,
    WorkspaceIndexHitKind, WorkspaceIndexSnapshot, workspace_id_from_path,
};

const MAX_DEPTH: usize = 6;
const MAX_ENTRIES: usize = 2000;
const MAX_RUST_FILE_BYTES: u64 = 1024 * 1024;

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".tiangong",
    "target",
    "node_modules",
    "__pycache__",
    ".next",
    "dist",
    "build",
];

pub(crate) fn query_current_workspace(
    workspace_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Vec<WorkspaceIndexHit> {
    let Ok(root) = std::env::current_dir() else {
        return Vec::new();
    };
    let resolved_workspace_id = workspace_id
        .map(str::to_string)
        .unwrap_or_else(|| workspace_id_from_path(&root));
    query_workspace_index_with_id(&root, &resolved_workspace_id, query, limit).unwrap_or_default()
}

pub fn refresh_workspace_index(root: &Path) -> Result<WorkspaceIndexSnapshot> {
    let workspace_id = workspace_id_from_path(root);
    refresh_workspace_index_with_id(root, &workspace_id)
}

pub fn query_workspace_index(
    root: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<WorkspaceIndexHit>> {
    let workspace_id = workspace_id_from_path(root);
    query_workspace_index_with_id(root, &workspace_id, query, limit)
}

pub fn touch_workspace_index_file(
    root: &Path,
    relative_or_absolute_path: &Path,
) -> Result<WorkspaceIndexSnapshot> {
    let workspace_id = workspace_id_from_path(root);
    let mut snapshot = load_or_refresh(root, &workspace_id)?;
    let relative_path = normalize_relative_path(root, relative_or_absolute_path)?;
    let absolute_path = root.join(&relative_path);
    let relative_text = relative_path.to_string_lossy().replace('\\', "/");

    snapshot
        .file_tree
        .entries
        .retain(|entry| entry.path != relative_text);
    snapshot
        .symbols
        .symbols
        .retain(|symbol| symbol.file_path != relative_text);

    if absolute_path.exists() {
        if let Some(entry) = build_entry(root, &absolute_path)? {
            snapshot.file_tree.entries.push(entry);
            snapshot
                .file_tree
                .entries
                .sort_by(|left, right| left.path.cmp(&right.path));
        }
        if absolute_path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            snapshot
                .symbols
                .symbols
                .extend(extract_rust_symbols(root, &absolute_path)?);
            snapshot.symbols.symbols.sort_by(|left, right| {
                left.file_path
                    .cmp(&right.file_path)
                    .then(left.line_range.0.cmp(&right.line_range.0))
            });
        }
    }

    let now = now_text();
    snapshot.file_tree.updated_at = now.clone();
    snapshot.symbols.updated_at = now;
    save_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub(crate) fn refresh_workspace_index_with_id(
    root: &Path,
    workspace_id: &str,
) -> Result<WorkspaceIndexSnapshot> {
    let root = root
        .canonicalize()
        .with_context(|| format!("解析工作区路径失败：{}", root.display()))?;
    let mut entries = Vec::new();
    collect_entries(&root, &root, 0, &mut entries)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));

    let rust_files = entries
        .iter()
        .filter(|entry| {
            entry.entry_type == FileType::File && entry.path.to_ascii_lowercase().ends_with(".rs")
        })
        .map(|entry| root.join(&entry.path))
        .collect::<Vec<_>>();

    let mut symbols = Vec::new();
    for path in rust_files {
        symbols.extend(extract_rust_symbols(&root, &path)?);
    }
    symbols.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then(left.line_range.0.cmp(&right.line_range.0))
    });

    let updated_at = now_text();
    let snapshot = WorkspaceIndexSnapshot {
        file_tree: FileTreeIndex {
            workspace_id: workspace_id.to_string(),
            root_path: root.to_string_lossy().to_string(),
            updated_at: updated_at.clone(),
            entries,
        },
        symbols: SymbolIndex {
            workspace_id: workspace_id.to_string(),
            updated_at,
            symbols,
        },
    };
    save_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn query_workspace_index_with_id(
    root: &Path,
    workspace_id: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<WorkspaceIndexHit>> {
    let snapshot = load_or_refresh(root, workspace_id)?;
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return Ok(Vec::new());
    }

    let mut hits = Vec::new();
    for entry in &snapshot.file_tree.entries {
        let haystack = entry.path.to_ascii_lowercase();
        let score = score_text(&haystack, &tokens);
        if score > 0 {
            hits.push(WorkspaceIndexHit {
                hit_kind: match entry.entry_type {
                    FileType::File => WorkspaceIndexHitKind::File,
                    FileType::Directory => WorkspaceIndexHitKind::Directory,
                },
                path: entry.path.clone(),
                name: None,
                symbol_kind: None,
                line: None,
                score: score as f64,
            });
        }
    }

    for symbol in &snapshot.symbols.symbols {
        let haystack = format!(
            "{} {} {}",
            symbol.name,
            symbol.file_path,
            symbol.signature.as_deref().unwrap_or_default()
        )
        .to_ascii_lowercase();
        let score = score_text(&haystack, &tokens);
        if score > 0 {
            hits.push(WorkspaceIndexHit {
                hit_kind: WorkspaceIndexHitKind::Symbol,
                path: symbol.file_path.clone(),
                name: Some(symbol.name.clone()),
                symbol_kind: Some(symbol.kind.clone()),
                line: Some(symbol.line_range.0),
                score: score as f64 + 0.5,
            });
        }
    }

    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then(left.path.cmp(&right.path))
    });
    hits.truncate(limit.max(1));
    Ok(hits)
}

fn load_or_refresh(root: &Path, workspace_id: &str) -> Result<WorkspaceIndexSnapshot> {
    let root = root
        .canonicalize()
        .with_context(|| format!("解析工作区路径失败：{}", root.display()))?;
    let snapshot = load_snapshot(workspace_id);
    match snapshot {
        Ok(snapshot) if Path::new(&snapshot.file_tree.root_path) == root => Ok(snapshot),
        _ => refresh_workspace_index_with_id(&root, workspace_id),
    }
}

fn collect_entries(
    root: &Path,
    current: &Path,
    depth: usize,
    entries: &mut Vec<FileEntry>,
) -> Result<()> {
    if depth > MAX_DEPTH || entries.len() >= MAX_ENTRIES {
        return Ok(());
    }

    let mut children = fs::read_dir(current)
        .with_context(|| format!("读取目录失败：{}", current.display()))?
        .filter_map(std::result::Result::ok)
        .collect::<Vec<_>>();
    children.sort_by_key(|entry| entry.path());

    for child in children {
        if entries.len() >= MAX_ENTRIES {
            break;
        }
        let path = child.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_dir() && IGNORED_DIRS.contains(&name) {
            continue;
        }
        if let Some(entry) = build_entry(root, &path)? {
            let is_dir = entry.entry_type == FileType::Directory;
            entries.push(entry);
            if is_dir {
                collect_entries(root, &path, depth + 1, entries)?;
            }
        }
    }
    Ok(())
}

fn build_entry(root: &Path, path: &Path) -> Result<Option<FileEntry>> {
    let metadata =
        fs::metadata(path).with_context(|| format!("读取文件元数据失败：{}", path.display()))?;
    let relative = match path.strip_prefix(root) {
        Ok(value) if !value.as_os_str().is_empty() => value,
        _ => return Ok(None),
    };
    let entry_type = if metadata.is_dir() {
        FileType::Directory
    } else {
        FileType::File
    };
    Ok(Some(FileEntry {
        path: relative.to_string_lossy().replace('\\', "/"),
        entry_type,
        size_bytes: metadata.len(),
        modified_at: system_time_text(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH)),
        annotation: None,
    }))
}

fn extract_rust_symbols(root: &Path, path: &Path) -> Result<Vec<SymbolEntry>> {
    let metadata =
        fs::metadata(path).with_context(|| format!("读取 Rust 文件失败：{}", path.display()))?;
    if metadata.len() > MAX_RUST_FILE_BYTES {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("读取 Rust 文件失败：{}", path.display()))?;
    let relative = normalize_relative_path(root, path)?
        .to_string_lossy()
        .replace('\\', "/");
    let mut symbols = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }
        if let Some((kind, name)) = parse_rust_symbol(trimmed) {
            symbols.push(SymbolEntry {
                name,
                kind,
                file_path: relative.clone(),
                line_range: (line_no, line_no),
                signature: Some(trimmed.chars().take(160).collect()),
            });
        }
    }
    Ok(symbols)
}

fn parse_rust_symbol(line: &str) -> Option<(SymbolKind, String)> {
    let mut rest = line.trim_start();
    loop {
        let next = rest
            .strip_prefix("pub(crate) ")
            .or_else(|| rest.strip_prefix("pub(super) "))
            .or_else(|| rest.strip_prefix("pub "))
            .or_else(|| rest.strip_prefix("async "))
            .or_else(|| rest.strip_prefix("unsafe "))
            .or_else(|| rest.strip_prefix("const "));
        match next {
            Some(value) => rest = value.trim_start(),
            None => break,
        }
    }

    for (prefix, kind) in [
        ("mod ", SymbolKind::Module),
        ("fn ", SymbolKind::Function),
        ("struct ", SymbolKind::Struct),
        ("enum ", SymbolKind::Enum),
        ("trait ", SymbolKind::Trait),
    ] {
        if let Some(value) = rest.strip_prefix(prefix) {
            let name = take_identifier(value);
            if !name.is_empty() {
                return Some((kind, name));
            }
        }
    }
    None
}

fn take_identifier(value: &str) -> String {
    value
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect()
}

fn normalize_relative_path(root: &Path, path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path
            .strip_prefix(root)
            .with_context(|| format!("路径不在工作区内：{}", path.display()))?
            .to_path_buf())
    } else {
        Ok(path.to_path_buf())
    }
}

fn load_snapshot(workspace_id: &str) -> Result<WorkspaceIndexSnapshot> {
    let dir = workspace_index_dir(workspace_id);
    let file_tree = serde_json::from_str(
        &fs::read_to_string(dir.join("file-tree.json"))
            .with_context(|| format!("读取工作区文件树索引失败：{workspace_id}"))?,
    )?;
    let symbols = serde_json::from_str(
        &fs::read_to_string(dir.join("symbols.json"))
            .with_context(|| format!("读取工作区符号索引失败：{workspace_id}"))?,
    )?;
    Ok(WorkspaceIndexSnapshot { file_tree, symbols })
}

fn save_snapshot(snapshot: &WorkspaceIndexSnapshot) -> Result<()> {
    let dir = workspace_index_dir(&snapshot.file_tree.workspace_id);
    fs::create_dir_all(&dir)
        .with_context(|| format!("创建工作区索引目录失败：{}", dir.display()))?;
    fs::write(
        dir.join("file-tree.json"),
        serde_json::to_string_pretty(&snapshot.file_tree)?,
    )?;
    fs::write(
        dir.join("symbols.json"),
        serde_json::to_string_pretty(&snapshot.symbols)?,
    )?;
    Ok(())
}

fn workspace_index_dir(workspace_id: &str) -> PathBuf {
    storage_root().join("workspace-index").join(workspace_id)
}

fn storage_root() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

fn now_text() -> String {
    chrono::Local::now().naive_local().to_string()
}

fn system_time_text(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(time)
        .naive_local()
        .to_string()
}

fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn score_text(haystack: &str, tokens: &[String]) -> usize {
    tokens
        .iter()
        .filter(|token| haystack.contains(*token))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn workspace_index_builds_file_tree_symbols_and_updates_incrementally() {
        let home = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let previous_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", home.path());
        }
        write_file(
            &workspace.path().join("src/lib.rs"),
            r#"
mod api;
pub struct MemoryStore;
pub enum MemoryMode { A }
pub trait RecallSource {}
pub async fn recall_workspace_index() {}
"#,
        );
        write_file(&workspace.path().join("README.md"), "workspace index");

        let snapshot = refresh_workspace_index(workspace.path()).unwrap();
        assert!(
            snapshot
                .file_tree
                .entries
                .iter()
                .any(|entry| entry.path == "src/lib.rs")
        );
        for name in [
            "api",
            "MemoryStore",
            "MemoryMode",
            "RecallSource",
            "recall_workspace_index",
        ] {
            assert!(
                snapshot
                    .symbols
                    .symbols
                    .iter()
                    .any(|symbol| symbol.name == name),
                "应提取 Rust 符号 {name}"
            );
        }

        let hits = query_workspace_index(workspace.path(), "recall workspace", 8).unwrap();
        assert!(
            hits.iter()
                .any(|hit| hit.name.as_deref() == Some("recall_workspace_index"))
        );

        write_file(
            &workspace.path().join("src/lib.rs"),
            "pub fn updated_symbol() {}\n",
        );
        let updated =
            touch_workspace_index_file(workspace.path(), Path::new("src/lib.rs")).unwrap();
        assert!(
            updated
                .symbols
                .symbols
                .iter()
                .any(|symbol| symbol.name == "updated_symbol")
        );
        assert!(
            !updated
                .symbols
                .symbols
                .iter()
                .any(|symbol| symbol.name == "MemoryStore")
        );
        unsafe {
            match previous_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
