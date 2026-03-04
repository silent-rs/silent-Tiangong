use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

const MAX_SCAN_FILES: usize = 240;
const MAX_SCAN_CHARS: usize = 120_000;

#[derive(Debug, Clone, Default)]
pub struct SkillConversionAnalysis {
    pub dependencies: Vec<String>,
    pub env_vars: Vec<String>,
}

pub fn analyze_external_skill(path: &Path) -> Result<SkillConversionAnalysis> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("skill 目录不存在或不可访问：{}", path.display()))?;
    if !canonical.is_dir() {
        return Err(anyhow!("skill 路径不是目录：{}", canonical.display()));
    }

    let mut dependencies = BTreeSet::new();
    let mut env_vars = BTreeSet::new();

    analyze_package_json(&canonical, &mut dependencies)?;
    analyze_requirements_txt(&canonical, &mut dependencies)?;
    analyze_pyproject_toml(&canonical, &mut dependencies)?;
    analyze_cargo_toml(&canonical, &mut dependencies)?;
    scan_env_candidates(&canonical, &mut env_vars)?;

    Ok(SkillConversionAnalysis {
        dependencies: dependencies.into_iter().collect(),
        env_vars: env_vars.into_iter().collect(),
    })
}

#[derive(Debug, Deserialize, Default)]
struct PackageJson {
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "peerDependencies")]
    peer_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    package_manager: String,
}

fn analyze_package_json(root: &Path, deps: &mut BTreeSet<String>) -> Result<()> {
    let path = root.join("package.json");
    if !path.is_file() {
        return Ok(());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("读取失败：{}", path.display()))?;
    let parsed: PackageJson = serde_json::from_str(&raw)
        .with_context(|| format!("解析 package.json 失败：{}", path.display()))?;

    for (name, version) in parsed.dependencies {
        deps.insert(format_dependency("npm", &name, &version));
    }
    for (name, version) in parsed.dev_dependencies {
        deps.insert(format_dependency("npm-dev", &name, &version));
    }
    for (name, version) in parsed.peer_dependencies {
        deps.insert(format_dependency("npm-peer", &name, &version));
    }
    let manager = parsed.package_manager.trim();
    if !manager.is_empty() {
        deps.insert(format!("package-manager:{manager}"));
    }
    Ok(())
}

fn analyze_requirements_txt(root: &Path, deps: &mut BTreeSet<String>) -> Result<()> {
    let path = root.join("requirements.txt");
    if !path.is_file() {
        return Ok(());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("读取失败：{}", path.display()))?;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        deps.insert(format!("pip:{trimmed}"));
    }
    Ok(())
}

fn analyze_pyproject_toml(root: &Path, deps: &mut BTreeSet<String>) -> Result<()> {
    let path = root.join("pyproject.toml");
    if !path.is_file() {
        return Ok(());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("读取失败：{}", path.display()))?;
    let value = raw
        .parse::<toml::Value>()
        .with_context(|| format!("解析 pyproject.toml 失败：{}", path.display()))?;
    if let Some(items) = value
        .get("project")
        .and_then(|v| v.get("dependencies"))
        .and_then(toml::Value::as_array)
    {
        for item in items.iter().filter_map(toml::Value::as_str) {
            deps.insert(format!("pip:{item}"));
        }
    }
    Ok(())
}

fn analyze_cargo_toml(root: &Path, deps: &mut BTreeSet<String>) -> Result<()> {
    let path = root.join("Cargo.toml");
    if !path.is_file() {
        return Ok(());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("读取失败：{}", path.display()))?;
    let value = raw
        .parse::<toml::Value>()
        .with_context(|| format!("解析 Cargo.toml 失败：{}", path.display()))?;
    if let Some(table) = value.get("dependencies").and_then(toml::Value::as_table) {
        for key in table.keys() {
            deps.insert(format!("cargo:{key}"));
        }
    }
    Ok(())
}

fn scan_env_candidates(root: &Path, env_vars: &mut BTreeSet<String>) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).with_context(|| format!("读取目录失败：{}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(value) => value,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if should_skip_dir(&path) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !file_type.is_file() || !should_scan_file(&path) {
                continue;
            }
            if scanned >= MAX_SCAN_FILES {
                return Ok(());
            }
            scanned += 1;
            if let Ok(raw) = fs::read_to_string(&path) {
                let content = truncate_chars(&raw, MAX_SCAN_CHARS);
                extract_env_candidates(&content, env_vars);
            }
        }
    }
    Ok(())
}

fn should_skip_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|v| v.to_str())
        .is_some_and(|name| {
            name.eq_ignore_ascii_case("node_modules")
                || name.eq_ignore_ascii_case(".git")
                || name.eq_ignore_ascii_case("target")
                || name.eq_ignore_ascii_case(".next")
                || name.eq_ignore_ascii_case("dist")
                || name.eq_ignore_ascii_case("build")
                || name.eq_ignore_ascii_case(".venv")
                || name.eq_ignore_ascii_case("venv")
        })
}

fn should_scan_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
        return false;
    };
    if name.eq_ignore_ascii_case(".env") || name.eq_ignore_ascii_case(".env.example") {
        return true;
    }

    let Some(ext) = path.extension().and_then(|v| v.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "md" | "txt"
            | "json"
            | "ts"
            | "js"
            | "mjs"
            | "cjs"
            | "py"
            | "sh"
            | "toml"
            | "yaml"
            | "yml"
            | "env"
            | "cfg"
    )
}

fn truncate_chars(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }
    raw.chars().take(max_chars).collect::<String>()
}

fn extract_env_candidates(content: &str, out: &mut BTreeSet<String>) {
    collect_dot_access(content, "process.env.", out);
    collect_quoted_after(content, "process.env[", out);
    collect_quoted_call(content, "std::env::var(", out);
    collect_quoted_call(content, "std::env::var_os(", out);
    collect_quoted_call(content, "env::var(", out);
    collect_quoted_call(content, "os.getenv(", out);
    collect_quoted_after(content, "os.environ[", out);
    collect_braced_dollar(content, out);
    collect_plain_dollar(content, out);
    collect_env_assignments(content, out);
}

fn collect_dot_access(content: &str, marker: &str, out: &mut BTreeSet<String>) {
    let mut rest = content;
    while let Some(idx) = rest.find(marker) {
        let after = &rest[idx + marker.len()..];
        let key = take_ident(after);
        if !key.is_empty() {
            push_env_candidate(&key, out);
        }
        rest = after;
    }
}

fn collect_quoted_after(content: &str, marker: &str, out: &mut BTreeSet<String>) {
    let mut rest = content;
    while let Some(idx) = rest.find(marker) {
        let after = &rest[idx + marker.len()..];
        if let Some((value, _consumed)) = take_first_quoted(after) {
            push_env_candidate(&value, out);
        }
        rest = after;
    }
}

fn collect_quoted_call(content: &str, marker: &str, out: &mut BTreeSet<String>) {
    let mut rest = content;
    while let Some(idx) = rest.find(marker) {
        let after = &rest[idx + marker.len()..];
        if let Some((value, _consumed)) = take_first_quoted(after) {
            push_env_candidate(&value, out);
        }
        rest = after;
    }
}

fn collect_braced_dollar(content: &str, out: &mut BTreeSet<String>) {
    let mut rest = content;
    while let Some(idx) = rest.find("${") {
        let after = &rest[idx + 2..];
        if let Some(end) = after.find('}') {
            let candidate = &after[..end];
            push_env_candidate(candidate, out);
            rest = &after[end + 1..];
            continue;
        }
        break;
    }
}

fn collect_plain_dollar(content: &str, out: &mut BTreeSet<String>) {
    let chars = content.chars().collect::<Vec<_>>();
    let mut idx = 0usize;
    while idx < chars.len() {
        if chars[idx] != '$' {
            idx += 1;
            continue;
        }
        let mut end = idx + 1;
        while end < chars.len() {
            let ch = chars[end];
            if ch.is_ascii_alphanumeric() || ch == '_' {
                end += 1;
            } else {
                break;
            }
        }
        if end > idx + 1 {
            let key = chars[idx + 1..end].iter().collect::<String>();
            push_env_candidate(&key, out);
        }
        idx = end;
    }
}

fn collect_env_assignments(content: &str, out: &mut BTreeSet<String>) {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, _value)) = trimmed.split_once('=') else {
            continue;
        };
        push_env_candidate(key, out);
    }
}

fn take_ident(raw: &str) -> String {
    let mut ident = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            ident.push(ch);
        } else {
            break;
        }
    }
    ident
}

fn take_first_quoted(raw: &str) -> Option<(String, usize)> {
    let mut started = false;
    let mut quote = '\0';
    let mut escaped = false;
    let mut collected = String::new();
    for (idx, ch) in raw.char_indices() {
        if !started {
            if ch == '\'' || ch == '"' {
                quote = ch;
                started = true;
            } else if ch.is_whitespace() || ch == '[' || ch == '(' {
                continue;
            } else {
                return None;
            }
            continue;
        }

        if escaped {
            collected.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return Some((collected, idx));
        }
        collected.push(ch);
    }
    None
}

fn push_env_candidate(raw: &str, out: &mut BTreeSet<String>) {
    let candidate = raw.trim();
    if !looks_like_env_key(candidate) {
        return;
    }
    out.insert(candidate.to_string());
}

fn looks_like_env_key(raw: &str) -> bool {
    if raw.is_empty() || raw.len() > 80 {
        return false;
    }
    let mut has_alpha = false;
    for (idx, ch) in raw.chars().enumerate() {
        if idx == 0 && !(ch.is_ascii_alphabetic() || ch == '_') {
            return false;
        }
        if !(ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_') {
            return false;
        }
        if ch.is_ascii_uppercase() {
            has_alpha = true;
        }
    }
    has_alpha
}

fn format_dependency(prefix: &str, name: &str, version: &str) -> String {
    let name = name.trim();
    let version = version.trim();
    if name.is_empty() {
        return prefix.to_string();
    }
    if version.is_empty() {
        format!("{prefix}:{name}")
    } else {
        format!("{prefix}:{name}@{version}")
    }
}
