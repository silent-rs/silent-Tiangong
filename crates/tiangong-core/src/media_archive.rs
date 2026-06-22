use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose};
use tiangong_types::{MediaAsset, MediaKind};

/// 把输入附件归档到本地（图片/文件）。
///
/// - 图片：归档到 `~/.tiangong/media/images/`
/// - PDF/Office 文档：归档到 `~/.tiangong/media/files/`
/// - 其他：保留原引用
pub fn archive_input_media_assets(media: Vec<MediaAsset>) -> Vec<MediaAsset> {
    media
        .into_iter()
        .map(|asset| {
            if !should_archive_input_asset(&asset) {
                return asset;
            }
            if is_document_asset(&asset) {
                match archive_file_reference(&asset.url, asset.mime_type.as_deref()) {
                    Ok(archived) => MediaAsset {
                        url: archived.path,
                        mime_type: Some(archived.mime_type),
                        ..asset
                    },
                    Err(err) => {
                        tracing::warn!(
                            url = %asset.url,
                            error = %err,
                            "输入文档归档到本地失败，保留原始引用"
                        );
                        asset
                    }
                }
            } else {
                match archive_image_reference(&asset.url, asset.mime_type.as_deref()) {
                    Ok(archived) => MediaAsset {
                        url: archived.path,
                        mime_type: Some(archived.mime_type),
                        ..asset
                    },
                    Err(err) => {
                        tracing::warn!(
                            url = %asset.url,
                            error = %err,
                            "输入图片归档到本地失败，保留原始引用"
                        );
                        asset
                    }
                }
            }
        })
        .collect()
}

fn should_archive_input_asset(asset: &MediaAsset) -> bool {
    if matches!(asset.kind, MediaKind::Image) {
        return true;
    }
    if matches!(asset.kind, MediaKind::File) {
        // 图片类文件走图片归档
        if asset
            .mime_type
            .as_deref()
            .is_some_and(|mime| mime.starts_with("image/"))
            || image_mime_from_reference(&asset.url).is_some()
        {
            return true;
        }
        // PDF/Office 文档走文件归档
        if is_document_asset(asset) {
            return true;
        }
    }
    false
}

/// 判断是否为可归档的文档附件（PDF/Word/Excel/PowerPoint 现代格式）
fn is_document_asset(asset: &MediaAsset) -> bool {
    if let Some(mime) = asset.mime_type.as_deref()
        && is_document_mime(mime)
    {
        return true;
    }
    document_mime_from_reference(&asset.url).is_some()
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

pub(crate) struct ArchivedImage {
    path: String,
    mime_type: String,
}

impl ArchivedImage {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}

pub(crate) fn archive_image_reference(
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

pub(crate) fn image_mime_from_reference(value: &str) -> Option<String> {
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
    let lower = value.trim().to_ascii_lowercase();
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
pub(crate) fn archive_file_reference(
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
        .ok_or_else(|| "无法识别文档类型".to_string())?;
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
        .ok_or_else(|| "data: 文档 MIME 不在支持范围内".to_string())?;
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
        .ok_or_else(|| "下载文档的 MIME 不在支持范围内".to_string())?;
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
}
