use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Schema, Value};
use tantivy::{Index, IndexWriter, TantivyDocument};

use super::tantivy_schema::{WorkspaceFields, workspace_schema};

const MAX_ENTRIES: usize = 5000;
const MAX_DEPTH: usize = 8;
const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;
const SNIPPET_LINES: usize = 50;

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

fn should_skip_dir(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

fn should_skip_file(name: &str) -> bool {
    SKIP_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
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

pub struct WorkspaceIndex {
    index: Index,
    writer: IndexWriter,
    fields: WorkspaceFields,
    #[allow(dead_code)]
    schema: Schema,
    root: PathBuf,
    entry_count: usize,
}

impl WorkspaceIndex {
    pub fn open_or_create(root: &Path) -> Result<Self> {
        let index_dir = Self::index_dir(root);
        let (schema, fields) = workspace_schema();

        let index = if index_dir.exists() {
            Index::open_in_dir(&index_dir).context("打开 Workspace Tantivy 索引失败")?
        } else {
            fs::create_dir_all(&index_dir).context("创建 Workspace 索引目录失败")?;
            Index::create_in_dir(&index_dir, schema.clone())
                .context("创建 Workspace Tantivy 索引失败")?
        };

        let writer = index
            .writer(15_000_000)
            .context("创建 Workspace 索引写入器失败")?;

        Ok(Self {
            schema,
            index,
            writer,
            fields,
            root: root.to_path_buf(),
            entry_count: 0,
        })
    }

    pub fn full_scan(&mut self) -> Result<usize> {
        self.writer
            .delete_all_documents()
            .context("清空 Workspace 索引失败")?;
        self.entry_count = 0;
        self.scan_dir(&self.root.clone(), 0)?;
        self.writer.commit().context("提交 Workspace 索引失败")?;
        Ok(self.entry_count)
    }

    fn scan_dir(&mut self, dir: &Path, depth: usize) -> Result<()> {
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

            if path.is_dir() {
                if should_skip_dir(&name_str) {
                    continue;
                }
                self.scan_dir(&path, depth + 1)?;
            } else if path.is_file() && !should_skip_file(&name_str) {
                let _ = self.index_file(&path);
            }
        }
        Ok(())
    }

    pub fn index_file(&mut self, path: &Path) -> Result<()> {
        let metadata = fs::metadata(path).ok();
        let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
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
        doc.add_text(self.fields.file_type, "file");
        doc.add_u64(self.fields.size, size);
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

        self.writer.add_document(doc)?;
        self.entry_count += 1;
        Ok(())
    }

    pub fn commit(&mut self) -> Result<()> {
        self.writer.commit().context("提交 Workspace 索引失败")?;
        Ok(())
    }

    pub fn remove_file(&mut self, rel_path: &str) -> Result<()> {
        let term = tantivy::Term::from_field_text(self.fields.path, rel_path);
        self.writer.delete_term(term);
        Ok(())
    }

    pub fn search(&self, query_text: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let reader = self
            .index
            .reader()
            .context("创建 Workspace 索引读取器失败")?;
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
            .context("解析 Workspace 搜索查询失败")?;
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

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

    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn index_dir(root: &Path) -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let workspace_id = md5_hex(root.to_string_lossy().as_bytes());
        home.join(".tiangong")
            .join("index")
            .join("workspaces")
            .join(workspace_id)
            .join("tantivy")
    }
}

pub struct SearchHit {
    pub path: String,
    pub language: String,
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
