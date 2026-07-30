//! 文本处理工具（自 `tiangong-memory` 下沉的纯逻辑）。
//!
//! 合并多个文件中重复的工具函数：去重、URL/路径提取、历史指代判定等。

use std::collections::HashSet;

/// 去重并清理字符串列表（忽略大小写、去除空白与空项）。
pub(crate) fn dedupe_strings(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert(item.to_lowercase()))
        .collect()
}

/// 是否包含历史指代词。
pub(crate) fn contains_history_reference(text: &str) -> bool {
    [
        "刚刚",
        "刚才",
        "之前",
        "上次",
        "继续",
        "那个",
        "这张图",
        "生成的图片",
        "previous",
        "earlier",
        "that one",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// 是否为精确锚点（URL / 路径 / 代码符号 / 文件扩展名）。
pub(crate) fn is_precise_anchor(item: &str) -> bool {
    item.contains("://")
        || item.starts_with('/')
        || item.starts_with("./")
        || item.starts_with("../")
        || item.contains("::")
        || item.contains(".rs")
        || item.contains(".md")
        || item.contains(".json")
        || item.contains(".png")
        || item.contains(".jpg")
        || item.contains(".mp4")
}

/// 从文本中提取 URL。
pub(crate) fn extract_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for prefix in ["https://", "http://", "data:image/"] {
        let mut start = 0;
        while let Some(offset) = text[start..].find(prefix) {
            let url_start = start + offset;
            let rest = &text[url_start..];
            let url_end = rest
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | '，' | '。' | '\\')
                })
                .unwrap_or(rest.len());
            let url = rest[..url_end].trim_matches(['"', '\'', ')', ']', '}', ',', '，', '。']);
            if !url.is_empty() {
                urls.push(url.to_string());
            }
            start = url_start + url_end;
        }
    }
    dedupe_strings(urls)
}

/// 从文本中提取文件路径。
pub(crate) fn extract_paths(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|token| {
            let cleaned = token
                .trim_matches(['"', '\'', ')', ']', '}', ',', '，', '。'])
                .strip_prefix("path=")
                .unwrap_or_else(|| token.trim_matches(['"', '\'', ')', ']', '}', ',', '，', '。']));
            let cleaned = cleaned
                .split(['"', '\'', ')', ']', '}', ',', '，', '。'])
                .next()
                .unwrap_or(cleaned);
            (cleaned.starts_with('/')
                || cleaned.starts_with("./")
                || cleaned.starts_with("../")
                || cleaned.contains(".rs")
                || cleaned.contains(".md")
                || cleaned.contains(".json")
                || cleaned.contains(".png")
                || cleaned.contains(".jpg")
                || cleaned.contains(".mp4"))
            .then(|| cleaned.to_string())
        })
        .collect::<Vec<_>>()
        .pipe(dedupe_strings)
}

/// 从文本中提取已知工具名。
pub(crate) fn extract_tool_names(text: &str) -> Vec<String> {
    let known = [
        "recall_memory",
        "generate_image",
        "write_file",
        "replace_in_file",
        "read_file",
        "run_command",
        "apply_patch",
        "cargo_test",
    ];
    known
        .iter()
        .filter(|name| text.contains(**name))
        .map(|name| (*name).to_string())
        .collect()
}

/// 从文本中提取媒体/产物标记。
pub(crate) fn extract_media_markers(text: &str) -> Vec<String> {
    let markers = [
        ("图片", "media"),
        ("图像", "media"),
        ("image", "media"),
        ("视频", "video"),
        ("video", "video"),
        ("文件", "file"),
        ("file", "file"),
        ("产物", "artifact"),
        ("artifact", "artifact"),
    ];
    markers
        .iter()
        .filter(|(marker, _)| text.contains(marker))
        .map(|(_, keyword)| (*keyword).to_string())
        .collect::<Vec<_>>()
        .pipe(dedupe_strings)
}

/// 从查询中提取文本词条（按标点/空白切分，长度 >= 2，排除历史指代）。
pub(crate) fn extract_text_terms(text: &str, limit: usize) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || "，。！？；：、,.!?;:'\"()（）".contains(c))
        .map(str::trim)
        .filter(|item| item.chars().count() >= 2)
        .filter(|item| !contains_history_reference(item))
        .take(limit)
        .map(String::from)
        .collect()
}

/// Pipe trait：链式调用工具。
pub(crate) trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}
