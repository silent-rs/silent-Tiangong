use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose};
use tiangong_types::{MediaAsset, MediaKind};

pub fn archive_input_media_assets(media: Vec<MediaAsset>) -> Vec<MediaAsset> {
    media
        .into_iter()
        .map(|asset| {
            if !should_archive_input_asset(&asset) {
                return asset;
            }
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
        })
        .collect()
}

fn should_archive_input_asset(asset: &MediaAsset) -> bool {
    matches!(asset.kind, MediaKind::Image)
        || matches!(asset.kind, MediaKind::File)
            && (asset
                .mime_type
                .as_deref()
                .is_some_and(|mime| mime.starts_with("image/"))
                || image_mime_from_reference(&asset.url).is_some())
}

struct ArchivedImage {
    path: String,
    mime_type: String,
}

fn archive_image_reference(
    reference: &str,
    mime_hint: Option<&str>,
) -> Result<ArchivedImage, String> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err("图片引用为空".to_string());
    }
    if is_archived_media_path(trimmed) {
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

fn is_archived_media_path(value: &str) -> bool {
    let path = Path::new(value);
    path.components()
        .collect::<Vec<_>>()
        .windows(3)
        .any(|parts| {
            parts[0].as_os_str() == ".tiangong"
                && parts[1].as_os_str() == "media"
                && parts[2].as_os_str() == "images"
        })
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
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "无法获取 HOME 目录".to_string())?;
    let dir = home.join(".tiangong").join("media").join("images");
    std::fs::create_dir_all(&dir).map_err(|err| format!("创建图片归档目录失败：{err}"))?;
    Ok(dir)
}

fn image_mime_from_reference(value: &str) -> Option<String> {
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
