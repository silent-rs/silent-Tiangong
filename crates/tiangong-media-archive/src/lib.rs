//! 输入附件本地归档
//!
//! 把用户输入的图片 / PDF / Office 文档归档到 `~/.tiangong/media/` 下，
//! 统一为本地路径引用。支持 data URL、http(s) 和本地路径三种来源。
//!
//! 原 `tiangong-core::media_archive`，已迁出为独立 crate（#208）。

use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose};
use tiangong_types::{MediaAsset, MediaKind};

/// 把输入附件归档到本地（图片/文件）。
/// 把输入附件归档到本地。
///
/// 新规则：**用户上传的图片和文件全部归档**。`MediaKind::Image` 走图片归档，
/// `MediaKind::File` 走文件归档。文件类型/MIME 只用于确定扩展名和展示信息，
/// 不决定是否归档。无法识别类型的文件以 `application/octet-stream` 保存，
/// 不再保留临时远程地址。
///
/// 文件名、标题不能覆盖明确的 `MediaKind`——图片标题为 `report.pdf` 时仍按图片处理。
pub fn archive_input_media_assets(media: Vec<MediaAsset>) -> Vec<MediaAsset> {
    media
        .into_iter()
        .map(|asset| match asset.kind {
            MediaKind::Image => match archive_image_reference(&asset.url, asset.mime_type.as_deref()) {
                Ok(archived) => MediaAsset {
                    url: archived.path,
                    mime_type: Some(archived.mime_type),
                    ..asset
                },
                Err(err) => {
                    tracing::warn!(url = %asset.url, error = %err, "输入图片归档到本地失败，保留原始引用");
                    asset
                }
            },
            // File（以及 Video/Audio 等其他类型）统一走文件归档。
            _ => {
                let mime_hint = asset
                    .mime_type
                    .clone()
                    .or_else(|| document_mime_from_reference(&asset.url))
                    .or_else(|| asset.title.as_deref().and_then(document_mime_from_reference));
                match archive_file_reference(&asset.url, mime_hint.as_deref()) {
                    Ok(archived) => MediaAsset {
                        url: archived.path,
                        mime_type: Some(archived.mime_type),
                        ..asset
                    },
                    Err(err) => {
                        tracing::warn!(url = %asset.url, error = %err, "输入文件归档到本地失败，保留原始引用");
                        asset
                    }
                }
            }
        })
        .collect()
}

fn is_document_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/pdf"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
    )
}

pub struct ArchivedImage {
    path: String,
    mime_type: String,
}

impl ArchivedImage {
    pub fn path(&self) -> &str {
        &self.path
    }
}

pub fn archive_image_reference(
    reference: &str,
    mime_hint: Option<&str>,
) -> Result<ArchivedImage, String> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err("图片引用为空".to_string());
    }
    if is_archived_media_path(trimmed, "images") {
        return Ok(ArchivedImage {
            path: trimmed.to_string(),
            mime_type: mime_hint
                .map(str::to_string)
                .or_else(|| image_mime_from_reference(trimmed))
                .unwrap_or_else(|| "image/png".to_string()),
        });
    }
    if trimmed.starts_with("data:image/") {
        return archive_data_image(trimmed);
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return download_remote_image(trimmed);
    }
    copy_local_image(Path::new(trimmed), mime_hint)
}

fn copy_local_image(path: &Path, mime_hint: Option<&str>) -> Result<ArchivedImage, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("读取本地图片失败：{err}"))?;
    let mime_type = mime_hint
        .filter(|mime| mime.starts_with("image/"))
        .map(str::to_string)
        .or_else(|| image_mime_from_reference(&path.to_string_lossy()))
        .unwrap_or_else(|| "image/png".to_string());
    write_archived_image(bytes, &mime_type)
}

fn archive_data_image(data_url: &str) -> Result<ArchivedImage, String> {
    let (header, data) = data_url
        .split_once(',')
        .ok_or_else(|| "data:image 缺少 base64 内容".to_string())?;
    let mime_type = header
        .strip_prefix("data:")
        .and_then(|raw| raw.split(';').next())
        .filter(|raw| raw.starts_with("image/"))
        .unwrap_or("image/png");
    let bytes = general_purpose::STANDARD
        .decode(data)
        .map_err(|err| format!("解码 data:image 失败：{err}"))?;
    write_archived_image(bytes, mime_type)
}

fn download_remote_image(url: &str) -> Result<ArchivedImage, String> {
    // reqwest::blocking::Client 内部创建 tokio 运行时，
    // 在 async 上下文中 drop 会 panic，用独立线程隔离。
    let url = url.to_string();
    std::thread::scope(|scope| {
        scope
            .spawn(|| download_remote_image_inner(&url))
            .join()
            .unwrap()
    })
}

fn download_remote_image_inner(url: &str) -> Result<ArchivedImage, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|err| format!("创建图片下载客户端失败：{err}"))?;
    let response = client
        .get(url)
        .send()
        .map_err(|err| format!("下载图片失败：{err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("下载图片失败：HTTP {status}"));
    }
    let mime_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| value.starts_with("image/"))
        .map(str::to_string)
        .or_else(|| image_mime_from_reference(url))
        .unwrap_or_else(|| "image/png".to_string());
    let bytes = response
        .bytes()
        .map_err(|err| format!("读取图片响应失败：{err}"))?
        .to_vec();
    write_archived_image(bytes, &mime_type)
}

fn write_archived_image(bytes: Vec<u8>, mime_type: &str) -> Result<ArchivedImage, String> {
    let dir = media_images_dir()?;
    let ext = image_ext_from_mime(mime_type).unwrap_or("png");
    let path = dir.join(format!("{}.{}", scru128::new(), ext));
    std::fs::write(&path, bytes).map_err(|err| format!("写入图片归档失败：{err}"))?;
    Ok(ArchivedImage {
        path: path.to_string_lossy().to_string(),
        mime_type: mime_type.to_string(),
    })
}

fn media_images_dir() -> Result<PathBuf, String> {
    media_subdir("images")
}

fn media_files_dir() -> Result<PathBuf, String> {
    media_subdir("files")
}

fn media_subdir(name: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "无法获取 HOME 目录".to_string())?;
    let dir = home.join(".tiangong").join("media").join(name);
    std::fs::create_dir_all(&dir).map_err(|err| format!("创建媒体归档目录失败：{err}"))?;
    Ok(dir)
}

/// 判断路径是否为已归档的媒体路径（images/files 子目录均识别）
fn is_archived_media_path(value: &str, subdir: &str) -> bool {
    let path = Path::new(value);
    path.components()
        .collect::<Vec<_>>()
        .windows(3)
        .any(|parts| {
            parts[0].as_os_str() == ".tiangong"
                && parts[1].as_os_str() == "media"
                && parts[2].as_os_str() == subdir
        })
}

/// 判断路径是否位于媒体归档目录（images 或 files）。
///
/// 后端用此验证前端传入的"已归档"路径确实位于媒体目录，不完全信任前端。
/// 接受正反斜杠（统一后判断）。
pub fn is_archived_media_path_any(value: &str) -> bool {
    let normalized = value.replace('\\', "/");
    is_archived_media_path(&normalized, "images") || is_archived_media_path(&normalized, "files")
}

pub fn image_mime_from_reference(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.starts_with("data:image/") {
        return lower
            .split_once(';')
            .map(|(mime, _)| mime.trim_start_matches("data:").to_string());
    }
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg".to_string())
    } else if lower.ends_with(".webp") {
        Some("image/webp".to_string())
    } else if lower.ends_with(".gif") {
        Some("image/gif".to_string())
    } else if lower.ends_with(".png") {
        Some("image/png".to_string())
    } else {
        None
    }
}

fn image_ext_from_mime(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/png" => Some("png"),
        _ => None,
    }
}

// ── 文档（PDF/Office）归档 ──────────────────────────────────────────────

/// 从文件引用推断文档 MIME（按扩展名）
fn document_mime_from_reference(value: &str) -> Option<String> {
    // 忽略查询参数与片段，只检查路径部分（如 "报告.pdf?token=xxx" → ".pdf"）。
    let path = value.split(['?', '#']).next().unwrap_or(value);
    let lower = path.trim().to_ascii_lowercase();
    if lower.ends_with(".pdf") {
        Some("application/pdf".to_string())
    } else if lower.ends_with(".docx") {
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string())
    } else if lower.ends_with(".xlsx") {
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string())
    } else if lower.ends_with(".pptx") {
        Some(
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string(),
        )
    } else {
        None
    }
}

/// 文档 MIME → 扩展名
fn document_ext_from_mime(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "application/pdf" => Some("pdf"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        _ => None,
    }
}

/// 把 PDF/Office 文档引用归档到 `~/.tiangong/media/files/`。
///
/// 支持 data URL、http(s) 和本地路径三种来源。
pub fn archive_file_reference(
    reference: &str,
    mime_hint: Option<&str>,
) -> Result<ArchivedImage, String> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err("文档引用为空".to_string());
    }
    if is_archived_media_path(trimmed, "files") {
        return Ok(ArchivedImage {
            path: trimmed.to_string(),
            mime_type: mime_hint
                .map(str::to_string)
                .or_else(|| document_mime_from_reference(trimmed))
                .unwrap_or_else(|| "application/octet-stream".to_string()),
        });
    }
    if let Some(rest) = trimmed.strip_prefix("data:") {
        return archive_data_file(rest, mime_hint);
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return download_remote_file(trimmed, mime_hint);
    }
    copy_local_file(Path::new(trimmed), mime_hint)
}

fn copy_local_file(path: &Path, mime_hint: Option<&str>) -> Result<ArchivedImage, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("读取本地文档失败：{err}"))?;
    let mime_type = mime_hint
        .filter(|mime| is_document_mime(mime))
        .map(str::to_string)
        .or_else(|| document_mime_from_reference(&path.to_string_lossy()))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    write_archived_file(bytes, &mime_type)
}

fn archive_data_file(data_payload: &str, mime_hint: Option<&str>) -> Result<ArchivedImage, String> {
    let (header, data) = data_payload
        .split_once(',')
        .ok_or_else(|| "data: 文档缺少 base64 内容".to_string())?;
    let mime_type = header
        .split(';')
        .next()
        .filter(|mime| is_document_mime(mime))
        .map(str::to_string)
        .or_else(|| {
            mime_hint
                .filter(|m| is_document_mime(m))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let bytes = general_purpose::STANDARD
        .decode(data)
        .map_err(|err| format!("解码 data: 文档失败：{err}"))?;
    write_archived_file(bytes, &mime_type)
}

fn download_remote_file(url: &str, mime_hint: Option<&str>) -> Result<ArchivedImage, String> {
    let url = url.to_string();
    std::thread::scope(|scope| {
        scope
            .spawn(move || download_remote_file_inner(&url, mime_hint))
            .join()
            .unwrap()
    })
}

fn download_remote_file_inner(url: &str, mime_hint: Option<&str>) -> Result<ArchivedImage, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|err| format!("创建文档下载客户端失败：{err}"))?;
    let response = client
        .get(url)
        .send()
        .map_err(|err| format!("下载文档失败：{err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("下载文档失败：HTTP {status}"));
    }
    let mime_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| is_document_mime(value))
        .map(str::to_string)
        .or_else(|| {
            mime_hint
                .filter(|m| is_document_mime(m))
                .map(str::to_string)
        })
        .or_else(|| document_mime_from_reference(url))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let bytes = response
        .bytes()
        .map_err(|err| format!("读取文档响应失败：{err}"))?
        .to_vec();
    write_archived_file(bytes, &mime_type)
}

fn write_archived_file(bytes: Vec<u8>, mime_type: &str) -> Result<ArchivedImage, String> {
    let dir = media_files_dir()?;
    let ext = document_ext_from_mime(mime_type).unwrap_or("bin");
    let path = dir.join(format!("{}.{}", scru128::new(), ext));
    std::fs::write(&path, bytes).map_err(|err| format!("写入文档归档失败：{err}"))?;
    Ok(ArchivedImage {
        path: path.to_string_lossy().to_string(),
        mime_type: mime_type.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_pdf_data_url_to_local_file() {
        // 构造一个最小合法 PDF 的 data URL
        let pdf_bytes = b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF";
        let b64 = general_purpose::STANDARD.encode(pdf_bytes);
        let data_url = format!("data:application/pdf;base64,{b64}");

        let asset = MediaAsset {
            kind: MediaKind::File,
            url: data_url,
            mime_type: Some("application/pdf".to_string()),
            title: Some("test.pdf".to_string()),
            capability: None,
        };

        let archived = archive_input_media_assets(vec![asset]);
        assert_eq!(archived.len(), 1);
        let result = &archived[0];

        // 归档后 url 必须是本地路径，不再是 data URL
        assert!(
            !result.url.starts_with("data:"),
            "归档后 url 仍为 data URL：{}",
            &result.url[..result.url.len().min(60)]
        );
        // 路径应指向 media/files/ 目录
        assert!(
            result.url.contains("media/files/"),
            "归档路径不在 media/files/ 下：{}",
            result.url
        );
        // 文件应真实存在
        assert!(
            Path::new(&result.url).exists(),
            "归档文件不存在：{}",
            result.url
        );
        // 清理
        let _ = std::fs::remove_file(&result.url);
    }

    #[test]
    fn archives_docx_by_mime_even_without_extension_in_url() {
        // data URL 没有 .docx 扩展名，但 mime_type 正确，应能归档
        let docx_bytes = b"PK\x03\x04fake_docx_payload";
        let b64 = general_purpose::STANDARD.encode(docx_bytes);
        let data_url = format!(
            "data:application/vnd.openxmlformats-officedocument.wordprocessingml.document;base64,{b64}"
        );

        let asset = MediaAsset {
            kind: MediaKind::File,
            url: data_url,
            mime_type: Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    .to_string(),
            ),
            title: None,
            capability: None,
        };

        let archived = archive_input_media_assets(vec![asset]);
        assert_eq!(archived.len(), 1);
        assert!(!archived[0].url.starts_with("data:"));
        assert!(archived[0].url.ends_with(".docx"));
        let _ = std::fs::remove_file(&archived[0].url);
    }

    #[test]
    fn image_assets_still_archive_to_images_dir() {
        // 确保图片归档逻辑未被文件归档改动破坏
        let png_bytes = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        ];
        let b64 = general_purpose::STANDARD.encode(png_bytes);
        let data_url = format!("data:image/png;base64,{b64}");

        let asset = MediaAsset {
            kind: MediaKind::Image,
            url: data_url,
            mime_type: Some("image/png".to_string()),
            title: None,
            capability: None,
        };

        let archived = archive_input_media_assets(vec![asset]);
        assert_eq!(archived.len(), 1);
        assert!(
            archived[0].url.contains("media/images/"),
            "图片应归档到 media/images/：{}",
            archived[0].url
        );
        let _ = std::fs::remove_file(&archived[0].url);
    }

    #[test]
    fn unknown_file_type_archives_as_octet_stream() {
        // 无扩展名、无 MIME 的 data URL 文件应归档为 octet-stream，不报错
        let bytes = b"random binary content";
        let b64 = general_purpose::STANDARD.encode(bytes);
        let data_url = format!("data:application/octet-stream;base64,{b64}");
        let asset = MediaAsset {
            kind: MediaKind::File,
            url: data_url,
            mime_type: None,
            title: Some("unknown.dat".to_string()),
            capability: None,
        };
        let archived = archive_input_media_assets(vec![asset]);
        assert_eq!(archived.len(), 1);
        assert!(
            archived[0].url.contains("media/files/"),
            "未知类型文件应归档到 media/files/：{}",
            archived[0].url
        );
        assert!(
            archived[0].mime_type.as_deref() == Some("application/octet-stream"),
            "未知类型应为 octet-stream，实际：{:?}",
            archived[0].mime_type
        );
        let _ = std::fs::remove_file(&archived[0].url);
    }

    #[test]
    fn image_with_pdf_title_still_archives_as_image() {
        // 图片标题为 report.pdf 时仍按图片归档（title 不覆盖 MediaKind）
        let bytes = b"fake png";
        let b64 = general_purpose::STANDARD.encode(bytes);
        let data_url = format!("data:image/png;base64,{b64}");
        let asset = MediaAsset {
            kind: MediaKind::Image,
            url: data_url,
            mime_type: Some("image/png".to_string()),
            title: Some("report.pdf".to_string()),
            capability: None,
        };
        let archived = archive_input_media_assets(vec![asset]);
        assert_eq!(archived.len(), 1);
        assert!(
            archived[0].url.contains("media/images/"),
            "图片标题为 pdf 时仍应归档到 images/：{}",
            archived[0].url
        );
        let _ = std::fs::remove_file(&archived[0].url);
    }

    #[test]
    fn document_mime_from_reference_ignores_query_params() {
        // 带查询参数的 URL 应忽略参数后检查扩展名
        assert_eq!(
            document_mime_from_reference("https://host/dl/报告.pdf?token=xxx"),
            Some("application/pdf".to_string())
        );
        // 无扩展名的临时地址
        assert!(document_mime_from_reference("https://host/download/123").is_none());
    }

    #[test]
    fn is_archived_media_path_any_handles_backslashes() {
        // Unix 路径
        assert!(is_archived_media_path_any(
            "/home/user/.tiangong/media/images/abc.png"
        ));
        // Windows 路径（反斜杠）
        assert!(is_archived_media_path_any(
            "C:\\Users\\user\\.tiangong\\media\\files\\doc.pdf"
        ));
        // 非归档路径
        assert!(!is_archived_media_path_any("/tmp/some_file.png"));
    }
}
