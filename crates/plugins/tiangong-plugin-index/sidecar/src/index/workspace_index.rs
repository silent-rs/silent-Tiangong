use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Schema, Value};
use tantivy::{Index, IndexWriter, TantivyDocument};

use super::WORKSPACE_SCHEMA_VERSION;
use super::tantivy_schema::{WorkspaceFields, workspace_schema};

const MAX_ENTRIES: usize = 5000;
const MAX_DEPTH: usize = 8;
const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;
const SNIPPET_LINES: usize = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileState {
    modified_at: u64,
    size: u64,
}

#[derive(Default)]
struct ScanSnapshot {
    files: HashMap<String, FileState>,
    complete: bool,
}

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    ".cache",
    ".gradle",
    ".idea",
    ".vscode",
    ".next",
    ".nuxt",
    "dist",
    "build",
    "out",
    "vendor",
    "Pods",
    ".tox",
    ".venv",
    "venv",
    ".env",
    "coverage",
    ".terraform",
];

const SKIP_EXTENSIONS: &[&str] = &[
    ".o", ".obj", ".exe", ".dll", ".so", ".dylib", ".a", ".lib", ".class", ".jar", ".war", ".zip",
    ".tar", ".gz", ".bz2", ".xz", ".7z", ".rar", ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".ico",
    ".webp", ".woff", ".woff2", ".ttf", ".eot", ".otf", ".mp3", ".mp4", ".avi", ".mov", ".mkv",
    ".flv", ".wav", ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".db", ".sqlite",
    ".lock", ".log",
];

/// 跳过目录集合（OnceLock 惰性初始化，O(1) 查询）。
fn skip_dirs() -> &'static HashSet<&'static str> {
    static SKIP_DIRS_SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SKIP_DIRS_SET.get_or_init(|| SKIP_DIRS.iter().copied().collect())
}

/// 跳过扩展名集合（不含点，小写）。
fn skip_extensions() -> &'static HashSet<&'static str> {
    static SKIP_EXTENSIONS_SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SKIP_EXTENSIONS_SET.get_or_init(|| {
        SKIP_EXTENSIONS
            .iter()
            .copied()
            .map(|e| e.trim_start_matches('.'))
            .collect()
    })
}

fn should_skip_dir(name: &str) -> bool {
    name.starts_with('.') || skip_dirs().contains(name)
}

fn should_skip_file(name: &str) -> bool {
    // 用扩展名提取 + HashSet O(1) 查询替代 ~40 项 ends_with 线性比较。
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| skip_extensions().contains(ext))
}

fn detect_language(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "vue" => "vue",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" | "less" => "css",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" => "markdown",
        "sql" => "sql",
        "sh" | "bash" => "shell",
        _ => "",
    }
    .to_string()
}

fn read_snippet(path: &Path) -> String {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| {
            let lines: Vec<&str> = content.lines().take(SNIPPET_LINES).collect();
            if lines.is_empty() {
                None
            } else {
                Some(lines.join("\n"))
            }
        })
        .unwrap_or_default()
}

fn extract_rust_symbols(content: &str) -> Vec<SymbolEntry> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = (i + 1) as u64;

        if let Some(rest) = trimmed
            .strip_prefix("pub mod ")
            .or_else(|| trimmed.strip_prefix("mod "))
        {
            let name = rest.trim_end_matches(';').trim();
            if !name.is_empty() && !name.contains('{') {
                symbols.push(SymbolEntry {
                    name: name.to_string(),
                    kind: "module".to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: String::new(),
                });
            }
        }
        if let Some(rest) = trimmed
            .strip_prefix("pub fn ")
            .or_else(|| trimmed.strip_prefix("pub async fn "))
            .or_else(|| trimmed.strip_prefix("fn "))
            .or_else(|| trimmed.strip_prefix("async fn "))
            && let Some(name) = rest.split('(').next()
        {
            let name = name.trim();
            if !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
            {
                let sig = rest.split('{').next().unwrap_or(rest).trim().to_string();
                symbols.push(SymbolEntry {
                    name: name.to_string(),
                    kind: "function".to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: sig,
                });
            }
        }
        if let Some(rest) = trimmed
            .strip_prefix("pub struct ")
            .or_else(|| trimmed.strip_prefix("struct "))
        {
            let name = rest
                .split('<')
                .next()
                .unwrap_or(rest)
                .split('{')
                .next()
                .unwrap_or(rest)
                .trim_end_matches(';')
                .trim();
            if !name.is_empty() {
                symbols.push(SymbolEntry {
                    name: name.to_string(),
                    kind: "struct".to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: String::new(),
                });
            }
        }
        if let Some(rest) = trimmed
            .strip_prefix("pub enum ")
            .or_else(|| trimmed.strip_prefix("enum "))
        {
            let name = rest
                .split('<')
                .next()
                .unwrap_or(rest)
                .split('{')
                .next()
                .unwrap_or(rest)
                .trim_end_matches(';')
                .trim();
            if !name.is_empty() {
                symbols.push(SymbolEntry {
                    name: name.to_string(),
                    kind: "enum".to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: String::new(),
                });
            }
        }
        if let Some(rest) = trimmed
            .strip_prefix("pub trait ")
            .or_else(|| trimmed.strip_prefix("trait "))
        {
            let name = rest
                .split('<')
                .next()
                .unwrap_or(rest)
                .split('{')
                .next()
                .unwrap_or(rest)
                .split(':')
                .next()
                .unwrap_or(rest)
                .trim();
            if !name.is_empty() {
                symbols.push(SymbolEntry {
                    name: name.to_string(),
                    kind: "trait".to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: String::new(),
                });
            }
        }
        if let Some(rest) = trimmed
            .strip_prefix("pub const ")
            .or_else(|| trimmed.strip_prefix("const "))
        {
            let name = rest.split(':').next().unwrap_or(rest).trim();
            if !name.is_empty() {
                symbols.push(SymbolEntry {
                    name: name.to_string(),
                    kind: "constant".to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: String::new(),
                });
            }
        }
        if let Some(rest) = trimmed.strip_prefix("impl ") {
            let name = rest
                .split('<')
                .next()
                .unwrap_or(rest)
                .split('{')
                .next()
                .unwrap_or(rest)
                .trim_end_matches(';')
                .trim();
            if !name.is_empty() {
                symbols.push(SymbolEntry {
                    name: name.to_string(),
                    kind: "impl".to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: String::new(),
                });
            }
        }
    }
    symbols
}

struct SymbolEntry {
    name: String,
    kind: String,
    line_start: u64,
    line_end: u64,
    signature: String,
}

#[allow(dead_code)]
pub struct WorkspaceIndex {
    index: Index,
    fields: WorkspaceFields,
    #[allow(dead_code)]
    schema: Schema,
    root: PathBuf,
    base_dir: PathBuf,
    entry_count: usize,
}

impl WorkspaceIndex {
    pub fn open_or_create(root: &Path, base_dir: &Path) -> Result<Self> {
        let index_dir = Self::index_dir(root, base_dir);
        let (schema, fields) = workspace_schema();

        let existed = index_dir.exists();
        let index = if existed {
            match Index::open_in_dir(&index_dir).with_context(|| {
                workspace_index_context(root, base_dir, "open", "打开 Workspace Tantivy 索引失败")
            }) {
                Ok(index) if index.schema() == schema => index,
                Ok(_) => {
                    tracing::info!(
                        workspace = %root.display(),
                        index_dir = %index_dir.display(),
                        "Workspace 索引 schema 已更新，准备全量校准"
                    );
                    fs::remove_dir_all(&index_dir).with_context(|| {
                        workspace_index_context(
                            root,
                            base_dir,
                            "migrate_schema",
                            "删除旧 Workspace 索引目录失败",
                        )
                    })?;
                    fs::create_dir_all(&index_dir).with_context(|| {
                        workspace_index_context(
                            root,
                            base_dir,
                            "migrate_schema",
                            "创建 Workspace 索引目录失败",
                        )
                    })?;
                    Index::create_in_dir(&index_dir, schema.clone()).with_context(|| {
                        workspace_index_context(
                            root,
                            base_dir,
                            "migrate_schema",
                            "创建新版 Workspace Tantivy 索引失败",
                        )
                    })?
                }
                Err(err) => {
                    tracing::warn!(
                        workspace = %root.display(),
                        index_dir = %index_dir.display(),
                        error = %err,
                        "Workspace Tantivy 索引打开失败，准备重建索引目录"
                    );
                    fs::remove_dir_all(&index_dir).with_context(|| {
                        workspace_index_context(
                            root,
                            base_dir,
                            "recover_open",
                            "删除损坏 Workspace 索引目录失败",
                        )
                    })?;
                    fs::create_dir_all(&index_dir).with_context(|| {
                        workspace_index_context(
                            root,
                            base_dir,
                            "recover_open",
                            "创建 Workspace 索引目录失败",
                        )
                    })?;
                    Index::create_in_dir(&index_dir, schema.clone()).with_context(|| {
                        workspace_index_context(
                            root,
                            base_dir,
                            "recover_open",
                            "重建 Workspace Tantivy 索引失败",
                        )
                    })?
                }
            }
        } else {
            fs::create_dir_all(&index_dir).with_context(|| {
                workspace_index_context(root, base_dir, "create", "创建 Workspace 索引目录失败")
            })?;
            Index::create_in_dir(&index_dir, schema.clone()).with_context(|| {
                workspace_index_context(root, base_dir, "create", "创建 Workspace Tantivy 索引失败")
            })?
        };

        let entry_count = index
            .reader()
            .with_context(|| workspace_index_context(root, base_dir, "open", "创建索引读取器失败"))?
            .searcher()
            .num_docs() as usize;
        Ok(Self {
            schema,
            index,
            fields,
            root: root.to_path_buf(),
            base_dir: base_dir.to_path_buf(),
            entry_count,
        })
    }

    #[allow(dead_code)]
    pub fn full_scan(&mut self) -> Result<usize> {
        let mut writer = self.create_writer("full_scan")?;
        writer
            .delete_all_documents()
            .with_context(|| self.context("full_scan", "清空 Workspace 索引失败"))?;
        self.entry_count = 0;
        self.scan_dir(&mut writer, &self.root.clone(), 0)?;
        writer
            .commit()
            .with_context(|| self.context("full_scan", "提交 Workspace 索引失败"))?;
        self.refresh_entry_count()?;
        self.write_meta()?;
        Ok(self.entry_count)
    }

    /// 构建全新索引到临时目录，完成后原子替换正式目录。
    ///
    /// 与 [`full_scan`] 的区别：构建期间不影响正在服务的旧索引（旧 `WorkspaceIndex`
    /// 实例仍持有旧 `tantivy::Index`，并发搜索继续命中旧数据）。完成后删除旧目录、
    /// rename 临时目录为正式目录，调用方再重新 `open_or_create` 拿到指向新目录的实例。
    ///
    /// 返回新建索引的文档数。
    pub fn build_to_staging(root: &Path, base_dir: &Path) -> Result<usize> {
        let index_dir = Self::index_dir(root, base_dir);
        let build_dir = Self::build_dir(root, base_dir);
        let (schema, fields) = workspace_schema();

        // 清理可能残留的上次失败构建目录。
        if build_dir.exists() {
            let _ = fs::remove_dir_all(&build_dir);
        }
        fs::create_dir_all(&build_dir).with_context(|| {
            workspace_index_context(root, base_dir, "build_to_staging", "创建构建目录失败")
        })?;

        // 在临时目录创建全新索引并全量扫描。
        let index = Index::create_in_dir(&build_dir, schema.clone()).with_context(|| {
            workspace_index_context(root, base_dir, "build_to_staging", "创建临时索引失败")
        })?;
        let mut staging = Self {
            index,
            fields,
            schema,
            root: root.to_path_buf(),
            base_dir: base_dir.to_path_buf(),
            entry_count: 0,
        };
        let mut writer = staging.create_writer("build_to_staging")?;
        staging.scan_dir(&mut writer, &staging.root.clone(), 0)?;
        writer.commit().with_context(|| {
            workspace_index_context(root, base_dir, "build_to_staging", "提交临时索引失败")
        })?;
        staging.refresh_entry_count()?;
        staging.write_meta()?;
        let count = staging.entry_count;

        // writer 已 drop（释放 tantivy 写锁），安全替换目录。
        drop(staging);
        // 删除旧正式目录，rename 临时目录为正式目录。
        // 两次操作非原子，但旧索引数据已通过旧 WorkspaceIndex 实例的 mmap 持有，
        // 删除目录不影响正在进行的搜索（Unix 下已打开的 fd 不会因目录删除失效）。
        if index_dir.exists() {
            fs::remove_dir_all(&index_dir).with_context(|| {
                workspace_index_context(root, base_dir, "build_to_staging", "删除旧索引目录失败")
            })?;
        }
        fs::rename(&build_dir, &index_dir).with_context(|| {
            workspace_index_context(root, base_dir, "build_to_staging", "替换索引目录失败")
        })?;
        Ok(count)
    }

    pub fn incremental_scan(&mut self) -> Result<usize> {
        let existing = self.indexed_file_states()?;
        let mut snapshot = ScanSnapshot {
            files: HashMap::new(),
            complete: true,
        };
        self.collect_file_states(&self.root.clone(), 0, &mut snapshot)?;

        let mut writer = self.create_writer("incremental_scan")?;
        for (rel_path, state) in &snapshot.files {
            if existing.get(rel_path) == Some(state) {
                continue;
            }
            writer.delete_term(tantivy::Term::from_field_text(
                self.fields.path_exact,
                rel_path,
            ));
            self.index_file_with_writer(&mut writer, &self.root.join(rel_path), None)?;
        }
        if snapshot.complete {
            for rel_path in existing.keys() {
                if !snapshot.files.contains_key(rel_path) {
                    writer.delete_term(tantivy::Term::from_field_text(
                        self.fields.path_exact,
                        rel_path,
                    ));
                }
            }
        }
        writer
            .commit()
            .with_context(|| self.context("incremental_scan", "提交 Workspace 增量索引失败"))?;
        self.refresh_entry_count()?;
        self.write_meta()?;
        Ok(self.entry_count)
    }

    fn indexed_file_states(&self) -> Result<HashMap<String, FileState>> {
        let reader = self
            .index
            .reader()
            .with_context(|| self.context("incremental_scan", "创建 Workspace 索引读取器失败"))?;
        let searcher = reader.searcher();
        let mut states = HashMap::new();
        for segment_reader in searcher.segment_readers() {
            let store_reader = segment_reader
                .get_store_reader(1)
                .with_context(|| self.context("incremental_scan", "打开 Workspace 文档存储失败"))?;
            for doc_id in segment_reader.doc_ids_alive() {
                let doc: TantivyDocument = store_reader.get(doc_id)?;
                let Some(path) = doc
                    .get_first(self.fields.path_exact)
                    .and_then(|value| value.as_str())
                else {
                    continue;
                };
                let size = doc
                    .get_first(self.fields.size)
                    .and_then(|value| value.as_u64())
                    .unwrap_or_default();
                let modified_at = doc
                    .get_first(self.fields.modified_at)
                    .and_then(|value| value.as_u64())
                    .unwrap_or_default();
                states.insert(path.to_string(), FileState { modified_at, size });
            }
        }
        Ok(states)
    }

    fn collect_file_states(
        &self,
        dir: &Path,
        depth: usize,
        snapshot: &mut ScanSnapshot,
    ) -> Result<()> {
        if depth > MAX_DEPTH || snapshot.files.len() >= MAX_ENTRIES {
            snapshot.complete = false;
            return Ok(());
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                snapshot.complete = false;
                tracing::warn!(path = %dir.display(), %error, "Workspace 目录读取失败，保留未扫描的旧索引");
                return Ok(());
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    snapshot.complete = false;
                    tracing::warn!(path = %dir.display(), %error, "Workspace 目录项读取失败");
                    continue;
                }
            };
            if snapshot.files.len() >= MAX_ENTRIES {
                snapshot.complete = false;
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    snapshot.complete = false;
                    tracing::warn!(path = %path.display(), %error, "Workspace 文件类型读取失败");
                    continue;
                }
            };
            if file_type.is_dir() {
                if !should_skip_dir(&name_str) {
                    self.collect_file_states(&path, depth + 1, snapshot)?;
                }
                continue;
            }
            if !file_type.is_file() || should_skip_file(&name_str) {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    snapshot.complete = false;
                    tracing::warn!(path = %path.display(), %error, "Workspace 文件属性读取失败");
                    continue;
                }
            };
            if metadata.len() > MAX_FILE_SIZE {
                continue;
            }
            let rel_path = path
                .strip_prefix(&self.root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            snapshot.files.insert(
                rel_path,
                FileState {
                    modified_at: modified_timestamp(&metadata),
                    size: metadata.len(),
                },
            );
        }
        Ok(())
    }

    fn scan_dir(&mut self, writer: &mut IndexWriter, dir: &Path, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH || self.entry_count >= MAX_ENTRIES {
            return Ok(());
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            if self.entry_count >= MAX_ENTRIES {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // 用 DirEntry 缓存的 file_type 判定类型，避免 path.is_dir()/is_file() 各一次 stat。
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if should_skip_dir(&name_str) {
                    continue;
                }
                self.scan_dir(writer, &path, depth + 1)?;
            } else if file_type.is_file()
                && !should_skip_file(&name_str)
                && let Err(err) =
                    self.index_file_with_writer(writer, &path, entry.metadata().ok().as_ref())
            {
                tracing::warn!(
                    workspace = %self.root.display(),
                    path = %path.display(),
                    error = %err,
                    "Workspace 文件索引写入失败，已跳过该文件"
                );
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn index_file(&mut self, path: &Path) -> Result<()> {
        let mut writer = self.create_writer("index_file")?;
        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        writer.delete_term(tantivy::Term::from_field_text(
            self.fields.path_exact,
            &rel_path,
        ));
        self.index_file_with_writer(&mut writer, path, None)?;
        writer
            .commit()
            .with_context(|| self.context("index_file", "提交 Workspace 索引失败"))?;
        self.refresh_entry_count()?;
        self.write_meta()?;
        Ok(())
    }

    /// 写入单个文件到索引。`metadata` 为调用方已获取的元数据（scan_dir 会复用
    /// `DirEntry::metadata()`），传入 `None` 时此处自行 stat 兜底。
    fn index_file_with_writer(
        &mut self,
        writer: &mut IndexWriter,
        path: &Path,
        metadata: Option<&fs::Metadata>,
    ) -> Result<()> {
        let owned_metadata;
        let metadata = match metadata {
            Some(m) => m,
            None => {
                owned_metadata = fs::metadata(path).ok();
                match owned_metadata.as_ref() {
                    Some(m) => m,
                    None => return Ok(()),
                }
            }
        };
        let size = metadata.len();
        if size > MAX_FILE_SIZE {
            return Ok(());
        }

        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let language = detect_language(path);
        let content = read_snippet(path);

        let mut doc = TantivyDocument::new();
        doc.add_text(self.fields.path, &rel_path);
        doc.add_text(self.fields.path_exact, &rel_path);
        doc.add_text(self.fields.file_type, "file");
        doc.add_u64(self.fields.size, size);
        doc.add_u64(self.fields.modified_at, modified_timestamp(metadata));
        doc.add_text(self.fields.language, &language);
        if !content.is_empty() {
            doc.add_text(self.fields.content, &content);
        }

        // 符号索引
        if language == "rust" {
            let symbols = extract_rust_symbols(&content);
            for sym in symbols {
                doc.add_text(self.fields.symbol_name, &sym.name);
                doc.add_text(self.fields.symbol_kind, &sym.kind);
                doc.add_u64(self.fields.symbol_line_start, sym.line_start);
                doc.add_u64(self.fields.symbol_line_end, sym.line_end);
                if !sym.signature.is_empty() {
                    doc.add_text(self.fields.symbol_signature, &sym.signature);
                }
            }
        }

        writer.add_document(doc)?;
        self.entry_count += 1;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn remove_file(&mut self, rel_path: &str) -> Result<()> {
        let mut writer = self.create_writer("remove_file")?;
        let term = tantivy::Term::from_field_text(self.fields.path_exact, rel_path);
        writer.delete_term(term);
        writer
            .commit()
            .with_context(|| self.context("remove_file", "提交 Workspace 索引失败"))?;
        self.refresh_entry_count()?;
        self.write_meta()?;
        Ok(())
    }

    pub fn search(&self, query_text: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let reader = self
            .index
            .reader()
            .with_context(|| self.context("search", "创建 Workspace 索引读取器失败"))?;
        let searcher = reader.searcher();

        let path_field = self.fields.path;
        let content_field = self.fields.content;
        let symbol_name_field = self.fields.symbol_name;
        let symbol_signature_field = self.fields.symbol_signature;

        let query_parser = QueryParser::for_index(
            &self.index,
            vec![
                path_field,
                content_field,
                symbol_name_field,
                symbol_signature_field,
            ],
        );

        let query = query_parser
            .parse_query(query_text)
            .with_context(|| self.context("search", "解析 Workspace 搜索查询失败"))?;
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;

        let mut hits = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            let path_val = doc
                .get_first(self.fields.path)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let language = doc
                .get_first(self.fields.language)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            hits.push(SearchHit {
                path: path_val,
                language,
            });
        }
        Ok(hits)
    }

    #[allow(dead_code)]
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn refresh_entry_count(&mut self) -> Result<()> {
        self.entry_count = self
            .index
            .reader()
            .with_context(|| self.context("count", "创建 Workspace 索引读取器失败"))?
            .searcher()
            .num_docs() as usize;
        Ok(())
    }

    fn write_meta(&self) -> Result<()> {
        let now = chrono::Local::now();
        let meta = super::IndexMeta {
            root: self.root.to_string_lossy().to_string(),
            entry_count: self.entry_count,
            updated_at: now.naive_local().to_string(),
            schema_version: WORKSPACE_SCHEMA_VERSION,
            last_successful_scan_at: now.timestamp(),
        };
        let meta_dir = Self::index_dir(&self.root, &self.base_dir)
            .parent()
            .context("索引目录无效")?
            .to_path_buf();
        std::fs::create_dir_all(&meta_dir)
            .with_context(|| self.context("write_meta", "创建 Workspace meta 目录失败"))?;
        let meta_path = meta_dir.join("meta.json");
        let temp_path = meta_dir.join(format!("meta.json.tmp-{}", std::process::id()));
        let json = serde_json::to_string_pretty(&meta)
            .with_context(|| self.context("write_meta", "序列化 Workspace meta 失败"))?;
        std::fs::write(&temp_path, json)
            .with_context(|| self.context("write_meta", "写入 Workspace 临时 meta 失败"))?;
        std::fs::rename(&temp_path, &meta_path)
            .with_context(|| self.context("write_meta", "替换 Workspace meta 失败"))?;
        Ok(())
    }

    pub(crate) fn index_dir(root: &Path, base_dir: &Path) -> PathBuf {
        let workspace_id = md5_hex(root.to_string_lossy().as_bytes());
        base_dir
            .join("workspaces")
            .join(workspace_id)
            .join("tantivy")
    }

    /// 临时构建目录（rebuild 时构建新索引到此目录，完成后原子替换正式目录）。
    pub(crate) fn build_dir(root: &Path, base_dir: &Path) -> PathBuf {
        Self::index_dir(root, base_dir).with_file_name("tantivy.build")
    }

    fn create_writer(&self, stage: &str) -> Result<IndexWriter> {
        let mut last_error = None;
        for attempt in 1..=3 {
            match self.index.writer(15_000_000) {
                Ok(writer) => return Ok(writer),
                Err(err) => {
                    last_error = Some(anyhow!(err));
                    if attempt < 3 {
                        std::thread::sleep(Duration::from_millis(50 * attempt));
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("unknown writer error")))
            .with_context(|| self.context(stage, "创建 Workspace 索引写入器失败"))
    }

    fn context(&self, stage: &str, message: &str) -> String {
        workspace_index_context(&self.root, &self.base_dir, stage, message)
    }
}

pub struct SearchHit {
    pub path: String,
    pub language: String,
}

pub fn hash_path(root: &Path) -> String {
    md5_hex(root.to_string_lossy().as_bytes())
}

fn modified_timestamp(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or_default()
}

fn workspace_index_context(root: &Path, base_dir: &Path, stage: &str, message: &str) -> String {
    format!(
        "{message}: workspace={} index_dir={} stage={stage}",
        root.display(),
        WorkspaceIndex::index_dir(root, base_dir).display()
    )
}

fn md5_hex(data: &[u8]) -> String {
    use std::hash::Hasher;
    let mut hasher = fnv::FnvHasher::default();
    hasher.write(data);
    let hash = hasher.finish();
    format!("{:016x}", hash)
}

// 需要引入 fnv 或使用简单 hash
mod fnv {
    use std::hash::Hasher;
    pub struct FnvHasher(u64);
    impl Default for FnvHasher {
        fn default() -> Self {
            Self(0xcbf29ce484222325)
        }
    }
    impl Hasher for FnvHasher {
        fn finish(&self) -> u64 {
            self.0
        }
        fn write(&mut self, bytes: &[u8]) {
            let mut hash = self.0;
            for &b in bytes {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
            self.0 = hash;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_index_does_not_hold_writer_between_operations() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().join("workspace");
        let base_dir = temp.path().join("index");
        fs::create_dir_all(workspace.join("src"))?;
        fs::write(
            workspace.join("src").join("lib.rs"),
            "pub struct DemoIndex;\npub fn demo_symbol() {}\n",
        )?;

        let mut first = WorkspaceIndex::open_or_create(&workspace, &base_dir)?;
        assert_eq!(first.full_scan()?, 1);

        let mut second = WorkspaceIndex::open_or_create(&workspace, &base_dir)?;
        assert_eq!(second.full_scan()?, 1);

        let hits = first.search("demo_symbol", 5)?;
        assert!(
            hits.iter().any(|hit| hit.path == "src/lib.rs"),
            "workspace search should find indexed Rust symbol"
        );

        Ok(())
    }

    #[test]
    fn should_skip_uses_hashset() {
        // 被跳过的目录与扩展名
        assert!(should_skip_dir("node_modules"));
        assert!(should_skip_dir(".git"));
        assert!(should_skip_file("trace.log"));
        assert!(should_skip_file("binary.png"));
        // 正常源码不跳过
        assert!(!should_skip_dir("src"));
        assert!(!should_skip_file("lib.rs"));
        assert!(!should_skip_file("index.ts"));
        // 无扩展名文件不跳过
        assert!(!should_skip_file("Makefile"));
    }

    #[test]
    fn full_scan_skips_ignored_entries() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().join("workspace");
        let base_dir = temp.path().join("index");
        fs::create_dir_all(workspace.join("src"))?;
        fs::create_dir_all(workspace.join("node_modules").join("pkg"))?;
        fs::write(workspace.join("src").join("lib.rs"), "pub fn kept() {}\n")?;
        // 应被跳过：node_modules 下与 .log 扩展
        fs::write(
            workspace.join("node_modules").join("pkg").join("lib.rs"),
            "pub fn skipped() {}\n",
        )?;
        fs::write(workspace.join("trace.log"), "noise\n")?;

        let mut index = WorkspaceIndex::open_or_create(&workspace, &base_dir)?;
        assert_eq!(index.full_scan()?, 1, "只应索引 src/lib.rs 一个文件");

        let hits = index.search("skipped", 5)?;
        assert!(hits.is_empty(), "node_modules 内容不应进入索引");
        let hits = index.search("kept", 5)?;
        assert!(hits.iter().any(|h| h.path == "src/lib.rs"));
        Ok(())
    }
}
