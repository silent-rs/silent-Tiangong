use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, Term, Value};
use tantivy::{Index, IndexWriter, TantivyDocument};

use super::WORKSPACE_SCHEMA_VERSION;
use super::tantivy_schema::{WorkspaceFields, workspace_schema};

const MAX_ENTRIES: usize = 5000;
const MAX_DEPTH: usize = 8;
const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;
const SNIPPET_LINES: usize = 50;

/// 按 token 前缀匹配构造查询：`ment` 命中 `mentions`、`main` 命中 `main.rs` 的
/// `main` token。
///
/// 用 `FuzzyTermQuery` 的距离 0 前缀模式而非正则：tantivy-fst 的 Regex 语法受限
/// （不接受 `^` 锚点），且前缀走 term dictionary 范围扫描，比正则引擎快。
/// 查询词是分词后的单个 token，无需转义——不含 tantivy 查询语法字符。
fn prefix_query(term: &str, field: Field) -> FuzzyTermQuery {
    FuzzyTermQuery::new_prefix(Term::from_field_text(field, term), 0, false)
}

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
    /// 本次打开是否删除了旧索引并按新 schema 重建（此时索引一定为空）。
    rebuilt: bool,
}

impl WorkspaceIndex {
    /// 本次打开是否属于「删除旧索引后重建」。
    ///
    /// 调用方（`IndexManager`）据此登记待补扫：查询路径不经过 `set_workspace`
    /// 的后台刷新，若不在打开点标记，重建出的空索引没人负责填充。
    pub fn was_rebuilt(&self) -> bool {
        self.rebuilt
    }

    pub fn open_or_create(root: &Path, base_dir: &Path) -> Result<Self> {
        let index_dir = Self::index_dir(root, base_dir);
        let (schema, fields) = workspace_schema();

        let existed = index_dir.exists();
        // schema 升级或索引损坏时旧目录被整体删除、按新 schema 重建，此时索引
        // 一定为空。用该字段告知调用方去补一次全量扫描，否则索引会长期停留在
        // 空状态（表现为 `@` 提及检索不到任何文件）。
        let mut rebuilt = false;
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
                    rebuilt = true;
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
                    rebuilt = true;
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
        if rebuilt {
            // 作废父级 meta.json：它记录的是「旧索引的成功扫描」。不清掉的话，
            // workspace_index_age_secs 会认为当前 schema 版本已成功扫描过，
            // 从而把刚重建出的空索引当成健康索引——查询返回空却被当作"没有匹配"。
            // 删掉后 age_secs 返回 None，调用方据此补一次全量扫描。
            let meta_path = index_dir
                .parent()
                .map(|parent| parent.join("meta.json"))
                .unwrap_or_else(|| index_dir.join("meta.json"));
            if meta_path.exists()
                && let Err(error) = fs::remove_file(&meta_path)
            {
                tracing::warn!(
                    workspace = %root.display(),
                    meta = %meta_path.display(),
                    %error,
                    "作废旧 Workspace 索引 meta 失败，可能漏掉重建补扫"
                );
            }
        }
        Ok(Self {
            schema,
            index,
            fields,
            root: root.to_path_buf(),
            base_dir: base_dir.to_path_buf(),
            entry_count,
            rebuilt,
        })
    }

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
            if !file_type.is_file() {
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
        // 二进制/文档（扩展名在跳过清单里）只建 path 条目：不读内容、不提符号，
        // 让 `@` 提及能指向图片/PDF/Office 等文件；`index_search` 工具靠
        // has_content 过滤掉它们，不会稀释内容检索结果。
        let with_content = !should_skip_file(&path.to_string_lossy());
        let language = detect_language(path);
        let content = if with_content {
            read_snippet(path)
        } else {
            String::new()
        };

        let mut doc = TantivyDocument::new();
        doc.add_text(self.fields.path, &rel_path);
        doc.add_text(self.fields.path_exact, &rel_path);
        doc.add_text(self.fields.file_type, "file");
        doc.add_u64(self.fields.size, size);
        doc.add_u64(self.fields.modified_at, modified_timestamp(metadata));
        doc.add_text(self.fields.language, &language);
        doc.add_bool(self.fields.has_content, with_content);
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

        let parsed = query_parser
            .parse_query(query_text)
            .with_context(|| self.context("search", "解析 Workspace 搜索查询失败"))?;
        // 只返回读取过内容的命中：二进制/文档只建 path 条目（has_content=false），
        // 它们是给 `@` 提及用的，不该稀释内容检索结果。
        let query = BooleanQuery::new(vec![
            (Occur::Must, Box::new(parsed)),
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_bool(self.fields.has_content, true),
                    IndexRecordOption::Basic,
                )),
            ),
        ]);
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

    /// `@` 提及文件候选：只匹配 path 字段，多词 AND，浅优先（路径段数少者靠前）。
    ///
    /// 与 {@link search} 的区别：不过滤 has_content（图片/PDF/Office 也要能被
    /// `@` 到）、不查内容与符号、多词 AND 而非 OR、按 token 前缀匹配（见下）。
    ///
    /// 查询词经索引同一 tokenizer 分析后再匹配：`main.rs` 切成 `main`/`rs`
    /// 与建索引时一致；每个词按**前缀**匹配而非整词相等——用户找文件时输入的
    /// 是文件名片段（`ment` 找 `mentions.rs`、`s` 找 `src`），整词相等会让
    /// 这些最自然的输入全部返回空，面板于是只剩「无匹配」。
    /// 用 FuzzyTermQuery 的距离 0 前缀模式：查询词已是单个 token，无需转义，
    /// 也不会因含 `:` `/` `~` `^` 等 tantivy 语法字符而解析失败。
    pub fn search_paths(&self, query_text: &str, limit: usize) -> Result<Vec<SearchHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        // 空查询词不返回候选：刚唤出面板就罗列全部文件既昂贵也无意义，
        // 等用户输入至少一个词再检索。
        let path_field = self.fields.path;
        let terms = self.path_query_terms(query_text, path_field)?;
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let reader = self
            .index
            .reader()
            .with_context(|| self.context("search_paths", "创建 Workspace 索引读取器失败"))?;
        let searcher = reader.searcher();

        // path 是 TEXT 字段（默认 tokenizer 已小写化），按 token 前缀做 AND：
        // 输入 `设计 文档` 要求路径同时含以这两个词开头的 token。
        let query = BooleanQuery::new(
            terms
                .iter()
                .map(|term| {
                    (
                        Occur::Must,
                        Box::new(prefix_query(term, path_field)) as Box<dyn tantivy::query::Query>,
                    )
                })
                .collect(),
        );

        // TopDocs 只按分数取前 N，而 AND 下各文档分数相同、顺序不定，
        // 因此多取一些再在 Rust 侧按浅优先排序。
        let fetch = limit.saturating_mul(4).clamp(limit, 200);
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(fetch).order_by_score())
            .with_context(|| self.context("search_paths", "执行 Workspace 路径检索失败"))?;

        let mut hits: Vec<SearchHit> = top_docs
            .into_iter()
            .filter_map(|(_score, doc_address)| {
                let doc: TantivyDocument = searcher.doc(doc_address).ok()?;
                let path_val = doc
                    .get_first(self.fields.path)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if path_val.is_empty() {
                    return None;
                }
                Some(SearchHit {
                    path: path_val,
                    language: String::new(),
                })
            })
            .collect();

        // 浅优先：路径段数少者靠前，同级按字典序保证结果稳定。
        hits.sort_by(|a, b| {
            path_depth(&a.path)
                .cmp(&path_depth(&b.path))
                .then_with(|| a.path.cmp(&b.path))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// 把用户输入切成 path 字段的查询 token：与建索引用同一 tokenizer，
    /// 因此 `main.rs`、`Cargo.toml`、`src/lib` 的切分结果和索引侧一致。
    ///
    /// 直接 `split_whitespace` 会把 `main.rs` 当成一个整词，永远匹配不上被切成
    /// `main`/`rs` 的索引 token——这是「输入带后缀的文件名查不到」的原因。
    fn path_query_terms(&self, query_text: &str, path_field: Field) -> Result<Vec<String>> {
        let mut tokenizer = self
            .index
            .tokenizer_for_field(path_field)
            .with_context(|| self.context("search_paths", "获取 path 字段 tokenizer 失败"))?;
        let mut stream = tokenizer.token_stream(query_text);
        let mut terms: Vec<String> = Vec::new();
        // token_stream 逐个产出 token；text 已由默认 tokenizer 小写化。
        while stream.advance() {
            let text = stream.token().text.as_str();
            if !text.is_empty() {
                terms.push(text.to_string());
            }
        }
        Ok(terms)
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

/// 路径段数（浅优先排序用）：`src/main.rs` → 2，`main.rs` → 1。
fn path_depth(path: &str) -> usize {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .count()
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
    fn full_scan_indexes_all_but_only_text_gets_content() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().join("workspace");
        let base_dir = temp.path().join("index");
        fs::create_dir_all(workspace.join("src"))?;
        fs::create_dir_all(workspace.join("node_modules").join("pkg"))?;
        fs::write(workspace.join("src").join("lib.rs"), "pub fn kept() {}\n")?;
        // 应被跳过：node_modules 目录下
        fs::write(
            workspace.join("node_modules").join("pkg").join("lib.rs"),
            "pub fn skipped() {}\n",
        )?;
        // 二进制只建 path 条目：不进索引内容，但 `@` 提户要能指向它
        fs::write(workspace.join("trace.log"), "noise\n")?;

        let mut index = WorkspaceIndex::open_or_create(&workspace, &base_dir)?;
        // src/lib.rs（有内容）+ trace.log（仅路径）= 2 条
        assert_eq!(index.full_scan()?, 2, "源码与二进制都应建条目");
        let hits = index.search("skipped", 5)?;
        assert!(hits.is_empty(), "node_modules 内容不应进入索引");
        let hits = index.search("kept", 5)?;
        assert!(hits.iter().any(|h| h.path == "src/lib.rs"));
        // 二进制只有 path 条目，内容检索不应命中
        let hits = index.search("noise", 5)?;
        assert!(hits.is_empty(), "二进制内容不进入索引");
        // 但路径检索能指向它（mention 候选）
        let hits = index.search_paths("trace", 5)?;
        assert!(
            hits.iter().any(|h| h.path == "trace.log"),
            "mention 路径检索应能指向二进制文件"
        );
        Ok(())
    }

    #[test]
    fn open_or_create_reports_rebuild_after_schema_change() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().join("workspace");
        let base_dir = temp.path().join("index");
        fs::create_dir_all(workspace.join("src"))?;
        fs::write(workspace.join("src").join("lib.rs"), "pub fn kept() {}\n")?;

        // 首次创建：不是重建
        let mut first = WorkspaceIndex::open_or_create(&workspace, &base_dir)?;
        assert!(!first.was_rebuilt(), "首次创建不应报告重建");
        assert_eq!(first.full_scan()?, 1);

        // schema 相同再次打开：仍然不是重建，且数据还在
        let second = WorkspaceIndex::open_or_create(&workspace, &base_dir)?;
        assert!(!second.was_rebuilt(), "schema 未变不应报告重建");
        drop(second);

        // 模拟 schema 升级：改一个字段选项，使磁盘 schema 与当前 schema 不匹配
        let index_dir = WorkspaceIndex::index_dir(&workspace, &base_dir);
        let mut on_disk: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(index_dir.join("meta.json"))?)?;
        on_disk["schema"][0]["options"]["stored"] = serde_json::json!(false);
        fs::write(
            index_dir.join("meta.json"),
            serde_json::to_string_pretty(&on_disk)?,
        )?;

        let third = WorkspaceIndex::open_or_create(&workspace, &base_dir)?;
        assert!(third.was_rebuilt(), "schema 变更必须报告重建");
        assert_eq!(
            third.entry_count(),
            0,
            "重建后的索引必须为空——调用方据此补全量扫描"
        );
        Ok(())
    }

    #[test]
    fn search_paths_requires_all_terms_and_prefers_shallow() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().join("workspace");
        let base_dir = temp.path().join("index");
        fs::create_dir_all(workspace.join("src").join("deep"))?;
        fs::write(workspace.join("src").join("main.rs"), "fn main() {}\n")?;
        fs::write(
            workspace.join("src").join("deep").join("main.rs"),
            "fn deep_main() {}\n",
        )?;
        fs::write(workspace.join("docs.md"), "# 文档\n")?;

        let mut index = WorkspaceIndex::open_or_create(&workspace, &base_dir)?;
        index.full_scan()?;

        // 空查询词不返回候选：刚唤出面板时罗列全部文件既昂贵也无意义
        assert!(index.search_paths("", 10)?.is_empty());
        assert!(index.search_paths("   ", 10)?.is_empty());

        // 多词 AND：`src main` 要求路径同时含两个 token
        let hits = index.search_paths("src main", 10)?;
        assert_eq!(hits.len(), 2, "两层目录的 main.rs 都应命中");
        // 浅优先：src/main.rs 排在 src/deep/main.rs 之前
        assert_eq!(hits[0].path, "src/main.rs");
        assert_eq!(hits[1].path, "src/deep/main.rs");

        // 只含其一的词不应命中（AND 语义）
        assert!(index.search_paths("main docs", 10)?.is_empty());

        // limit 生效
        assert_eq!(index.search_paths("rs", 1)?.len(), 1);
        // limit 为 0 直接返回空
        assert!(index.search_paths("rs", 0)?.is_empty());
        Ok(())
    }

    /// 用户输入的是文件名**片段**，不是完整 token。
    ///
    /// 对应真实故障：`search_paths` 曾用 TermQuery 做整词相等匹配，导致
    /// `ment` 找不到 `mentions.rs`、`s` 找不到 `src/...`、`main.rs`（带后缀）
    /// 一个都命中不了——@ 面板于是只剩「无匹配」，看起来像索引坏了。
    #[test]
    fn search_paths_matches_token_prefixes_and_splits_query() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().join("workspace");
        let base_dir = temp.path().join("index");
        fs::create_dir_all(workspace.join("src"))?;
        fs::write(workspace.join("src").join("mentions.rs"), "// x\n")?;
        fs::write(workspace.join("src").join("other.rs"), "// y\n")?;
        fs::write(workspace.join("docs.md"), "# 文档\n")?;

        let mut index = WorkspaceIndex::open_or_create(&workspace, &base_dir)?;
        index.full_scan()?;

        // 前缀：`ment` 命中 `mentions` token，但不命中 `other`
        let hits = index.search_paths("ment", 10)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/mentions.rs");

        // 单个字母也能当前缀用（`s` 命中 src 段）
        assert!(!index.search_paths("s", 10)?.is_empty());

        // 查询词必须经同一 tokenizer：`mentions.rs` 被切成 mentions/rs，
        // 与索引侧一致，因此能命中；整词相等时代码把它当一个词，永远查不到
        let hits = index.search_paths("mentions.rs", 10)?;
        assert_eq!(hits.len(), 1, "带后缀的文件名必须能命中");
        assert_eq!(hits[0].path, "src/mentions.rs");

        // 前缀 AND：`src ment` 两个词都要求前缀命中
        assert_eq!(index.search_paths("src ment", 10)?.len(), 1);
        // 前缀不满足时不得命中
        assert!(index.search_paths("src zzz", 10)?.is_empty());

        // 大小写不敏感（tokenizer 小写化）
        assert_eq!(index.search_paths("MENTIONS", 10)?.len(), 1);
        Ok(())
    }
}
