//! 从 Chat Completions 响应中提取图片。
//!
//! 兼容两种常见返回形态：
//! - 形态一（markdown 文本）：content 是字符串，图片以 `![](url)` 嵌在里面。
//! - 形态二（多模态 part）：content 是数组，图片在 `{type:"image_url",image_url:{url}}` part 里。

use anyhow::{Result, anyhow};
use serde_json::Value;

/// 从 Chat Completions 响应 JSON 中提取所有图片引用（URL 或 base64 data URI）。
///
/// 返回的每个元素是一个可以直接交给 `archive_image_reference` 的原始引用：
/// `https://...`、`data:image/png;base64,...` 等。
pub fn extract_images(payload: &Value) -> Result<Vec<String>> {
    let message = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .ok_or_else(|| anyhow!("响应缺少 choices[0].message"))?;

    let content = message.get("content");
    let mut images = Vec::new();

    if let Some(content) = content {
        match content {
            // 形态二：content 是数组，遍历找 image part。
            Value::Array(parts) => {
                for part in parts {
                    collect_from_content_part(part, &mut images);
                }
            }
            // 形态一：content 是字符串，正则提取 markdown 图片 / 裸 URL / base64。
            Value::String(text) => {
                collect_from_text(text, &mut images);
            }
            _ => {}
        }
    }

    // 形态二变体：部分服务把图片放在 message.images[] 数组里（非标准）。
    #[allow(clippy::collapsible_if)]
    if images.is_empty() {
        if let Some(images_field) = message.get("images").and_then(Value::as_array) {
            for part in images_field {
                collect_from_content_part(part, &mut images);
            }
        }
    }

    if images.is_empty() {
        return Err(anyhow!(
            "响应未包含可识别的图片（content 既无 markdown 图片链接，也无 image part）"
        ));
    }

    Ok(images)
}

/// 从单个 content part 中提取图片引用。
///
/// 兼容标准 `{"type":"image_url","image_url":{"url":...}}` 和
/// 部分服务自定义的 `{"image":...}` 形式。
fn collect_from_content_part(part: &Value, images: &mut Vec<String>) {
    // 标准形式：image_url.url
    if let Some(url) = part
        .get("image_url")
        .and_then(|v| v.get("url"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        images.push(url.to_string());
        return;
    }
    // 非标准形式：直接 {"image":"https://..."}
    if let Some(url) = part.get("image").and_then(Value::as_str).filter(|s| {
        let t = s.trim();
        !t.is_empty()
            && (t.starts_with("http://")
                || t.starts_with("https://")
                || t.starts_with("data:image/"))
    }) {
        images.push(url.to_string());
    }
}

/// 从文本内容中提取图片引用。
///
/// 优先匹配 markdown 图片语法 `![alt](url)`，再兜底匹配裸 URL/base64 data URI。
fn collect_from_text(text: &str, images: &mut Vec<String>) {
    // markdown 图片语法：![描述](地址)
    let mut start = 0usize;
    while let Some(bang) = text[start..].find("![") {
        let abs = start + bang;
        let after_bang = abs + 2;
        let Some(close_bracket) = text[after_bang..].find("](") else {
            break;
        };
        let url_start = after_bang + close_bracket + 2;
        let Some(close_paren) = text[url_start..].find(')') else {
            break;
        };
        let url = text[url_start..url_start + close_paren].trim();
        if !url.is_empty() {
            images.push(url.to_string());
        }
        start = url_start + close_paren + 1;
    }

    if !images.is_empty() {
        return;
    }

    // 兜底：裸 URL 或 base64 data URI。
    // 用按行扫描的方式，避免引入正则库依赖。
    for token in text.split_whitespace() {
        if token.starts_with("https://") || token.starts_with("http://") {
            if looks_like_image_url(token) {
                images.push(
                    token
                        .trim_end_matches(['.', ',', ')', ']', '}'])
                        .to_string(),
                );
            }
        } else if let Some(rest) = token.strip_prefix("data:image/") {
            // base64 data URI 形如 data:image/png;base64,xxxx
            if rest.contains(";base64,") {
                images.push(token.to_string());
            }
        }
    }
}

/// 粗略判断 URL 是否指向图片。
fn looks_like_image_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or(&lower);
    path.ends_with(".png")
        || path.ends_with(".jpg")
        || path.ends_with(".jpeg")
        || path.ends_with(".webp")
        || path.ends_with(".gif")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_from_markdown_text() {
        let payload = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "![image](https://example.com/x.png)\n\n"
                }
            }]
        });
        let images = extract_images(&payload).unwrap();
        assert_eq!(images, vec!["https://example.com/x.png".to_string()]);
    }

    #[test]
    fn extract_from_content_array() {
        let payload = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "这是图"},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}}
                    ]
                }
            }]
        });
        let images = extract_images(&payload).unwrap();
        assert_eq!(images, vec!["data:image/png;base64,abc".to_string()]);
    }

    #[test]
    fn extract_from_images_field() {
        let payload = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "images": [
                        {"type": "image_url", "image_url": {"url": "https://cdn.example.com/img.webp"}}
                    ]
                }
            }]
        });
        let images = extract_images(&payload).unwrap();
        assert_eq!(images, vec!["https://cdn.example.com/img.webp".to_string()]);
    }

    #[test]
    fn no_image_returns_error() {
        let payload = json!({
            "choices": [{
                "message": {"role": "assistant", "content": "纯文本没有图片"}
            }]
        });
        assert!(extract_images(&payload).is_err());
    }
}
