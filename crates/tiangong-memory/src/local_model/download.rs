//! 内置模型下载：分文件断点续传 + 大小/sha256 校验 + 原子落盘。
//!
//! 目录布局（`<storage>/memory/models/`）：
//!
//! ```text
//! models/
//!   bge-base-zh-v1.5/        # 完整模型（全部文件校验通过后整体改名而来）
//!     .complete              # 校验完成标记（内容为 revision）
//!     model_quantized.onnx
//!     tokenizer.json ...
//!   .partial-bge-m3/         # 下载中（*.part 为未完成文件，可续传）
//!   .lock                    # 跨进程互斥
//! ```
//!
//! 下载源按顺序尝试（同一文件断线后换源续传）：
//!
//! 1. `TIANGONG_MEMORY_MODEL_MIRROR`（可多个，逗号分隔）；
//! 2. 天工官方 OSS（`<oss>/memory-models/`）；
//! 3. ModelScope（国内 CDN，同名仓库）；
//! 4. hf-mirror.com；
//! 5. huggingface.co。
//!
//! 基地址形式的源按 HuggingFace 路径拼接 `<base>/<repo>/resolve/<revision>/<path>`；
//! 含 `{repo}` 占位符的源按模板展开（支持 `{repo}` `{revision}` `{path}`）。
//! 无论来源，内容都按清单中的大小与 sha256 校验，因此镜像无需被信任。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use super::catalog::{LocalModelSpec, ModelFile};

/// 自定义镜像环境变量（逗号分隔的基地址）。
pub const MODEL_MIRROR_ENV: &str = "TIANGONG_MEMORY_MODEL_MIRROR";
/// 默认下载源（按顺序尝试）。
const DEFAULT_SOURCES: [&str; 4] = [
    "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com/memory-models",
    // ModelScope 同名仓库只有 master 分支；内容由 sha256 锁定，分支漂移会被校验拦下。
    "https://www.modelscope.cn/models/{repo}/resolve/master/{path}",
    "https://hf-mirror.com",
    "https://huggingface.co",
];
const COMPLETE_MARKER: &str = ".complete";
/// 单个文件的最大重试轮次（每轮依次尝试所有下载源，续传不从头开始）。
const MAX_ROUNDS: usize = 6;

/// 模型根目录。
pub(crate) fn models_dir() -> PathBuf {
    crate::paths::memory_data_dir().join("models")
}

/// 模型完整目录。
pub(crate) fn model_dir(root: &Path, spec: &LocalModelSpec) -> PathBuf {
    root.join(spec.id)
}

/// 模型是否已完整下载（存在校验完成标记且 revision 一致、文件大小齐全）。
///
/// 只做轻量检查（sha256 在下载时已校验），用于启动时快速判断。
pub(crate) fn is_installed(root: &Path, spec: &LocalModelSpec) -> bool {
    let dir = model_dir(root, spec);
    let marker = std::fs::read_to_string(dir.join(COMPLETE_MARKER)).unwrap_or_default();
    marker.trim() == spec.revision
        && spec.files().iter().all(|file| {
            std::fs::metadata(dir.join(file.local_name()))
                .map(|meta| meta.len() == file.size)
                .unwrap_or(false)
        })
}

/// 下载中已落盘的字节数（完成的文件 + `.part` 临时文件），跨进程可见，
/// 用于页面展示下载进度。
pub(crate) fn partial_bytes(root: &Path, spec: &LocalModelSpec) -> u64 {
    let partial = root.join(format!(".partial-{}", spec.id));
    spec.files()
        .iter()
        .map(|file| {
            let done = std::fs::metadata(partial.join(file.local_name()))
                .map(|meta| meta.len())
                .unwrap_or(0);
            let part = std::fs::metadata(partial.join(format!("{}.part", file.local_name())))
                .map(|meta| meta.len())
                .unwrap_or(0);
            done.max(part).min(file.size)
        })
        .sum()
}

fn sources() -> Vec<String> {
    let mut sources = std::env::var(MODEL_MIRROR_ENV)
        .unwrap_or_default()
        .split(',')
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    sources.extend(DEFAULT_SOURCES.iter().map(|value| value.to_string()));
    sources
}

fn file_url(source: &str, spec: &LocalModelSpec, file: &ModelFile) -> String {
    if source.contains("{repo}") {
        return source
            .replace("{repo}", spec.repo)
            .replace("{revision}", spec.revision)
            .replace("{path}", file.path);
    }
    format!(
        "{source}/{}/resolve/{}/{}",
        spec.repo, spec.revision, file.path
    )
}

/// 跨进程文件锁：同一存储根同时只允许一个进程下载。
struct DownloadLock {
    file: std::fs::File,
}

impl DownloadLock {
    fn acquire(root: &Path) -> Result<Option<Self>> {
        use fs2::FileExt;
        std::fs::create_dir_all(root)
            .with_context(|| format!("创建模型目录失败: {}", root.display()))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join(".lock"))
            .with_context(|| "打开模型下载锁失败")?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { file })),
            Err(_) => Ok(None),
        }
    }
}

impl Drop for DownloadLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// 确保模型已下载；已安装时立即返回。
///
/// 另一进程正在下载时返回错误（调用方稍后重试即可）。
pub(crate) async fn ensure_model(root: &Path, spec: &LocalModelSpec) -> Result<PathBuf> {
    ensure_model_from(root, spec, &sources(), Duration::from_secs(1)).await
}

/// 指定下载源与重试基准间隔（测试注入本地源、缩短等待）。
async fn ensure_model_from(
    root: &Path,
    spec: &LocalModelSpec,
    sources: &[String],
    retry_base: Duration,
) -> Result<PathBuf> {
    let dir = model_dir(root, spec);
    if is_installed(root, spec) {
        return Ok(dir);
    }
    let Some(_lock) = DownloadLock::acquire(root)? else {
        bail!("另一个 Memory 进程正在下载内置模型，稍后重试");
    };
    // 拿到锁后再检查一次：可能刚被其他进程下完。
    if is_installed(root, spec) {
        return Ok(dir);
    }

    let partial = root.join(format!(".partial-{}", spec.id));
    std::fs::create_dir_all(&partial)
        .with_context(|| format!("创建下载目录失败: {}", partial.display()))?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()
        .with_context(|| "创建模型下载客户端失败")?;
    for file in spec.files() {
        download_file(&client, sources, spec, &file, &partial, retry_base).await?;
    }

    std::fs::write(partial.join(COMPLETE_MARKER), spec.revision)
        .with_context(|| "写入模型完成标记失败")?;
    if dir.exists() {
        // 残缺的旧目录（revision 不符或缺文件），整体替换。
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("清理旧模型目录失败: {}", dir.display()))?;
    }
    std::fs::rename(&partial, &dir)
        .with_context(|| format!("落盘模型目录失败: {}", dir.display()))?;
    tracing::info!(model = spec.id, dir = %dir.display(), "Memory 内置模型下载完成");
    Ok(dir)
}

async fn download_file(
    client: &reqwest::Client,
    sources: &[String],
    spec: &LocalModelSpec,
    file: &ModelFile,
    partial: &Path,
    retry_base: Duration,
) -> Result<()> {
    let target = partial.join(file.local_name());
    if std::fs::metadata(&target)
        .map(|meta| meta.len() == file.size)
        .unwrap_or(false)
        && sha256_file(&target)? == file.sha256
    {
        return Ok(());
    }
    let part = partial.join(format!("{}.part", file.local_name()));
    let mut last_error = None;
    for round in 0..MAX_ROUNDS {
        for source in sources {
            let url = file_url(source, spec, file);
            match fetch_resume(client, &url, &part, file.size).await {
                Ok(()) => {
                    let actual = sha256_file(&part)?;
                    if actual != file.sha256 {
                        // 内容不符：丢弃重下，并换下一个来源。
                        let _ = std::fs::remove_file(&part);
                        last_error = Some(anyhow!(
                            "{} 校验失败（来源 {source}）：sha256 {actual} ≠ {}",
                            file.path,
                            file.sha256
                        ));
                        continue;
                    }
                    std::fs::rename(&part, &target)
                        .with_context(|| format!("落盘模型文件失败: {}", target.display()))?;
                    return Ok(());
                }
                Err(error) => {
                    tracing::debug!(
                        model = spec.id,
                        file = file.path,
                        source = %source,
                        round,
                        "Memory 模型文件下载中断，稍后续传: {error:#}"
                    );
                    last_error = Some(error);
                }
            }
        }
        if round + 1 < MAX_ROUNDS {
            tokio::time::sleep(retry_base * (round as u32 + 1).min(5)).await;
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("没有可用的下载源")))
        .with_context(|| format!("下载 {} 的 {} 失败", spec.id, file.path))
}

/// 从 `part` 当前长度续传到 `expected` 字节。
async fn fetch_resume(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    expected: u64,
) -> Result<()> {
    let mut offset = std::fs::metadata(part).map(|meta| meta.len()).unwrap_or(0);
    if offset > expected {
        std::fs::remove_file(part).ok();
        offset = 0;
    }
    if offset == expected {
        return Ok(());
    }
    let mut request = client.get(url);
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let response = request.send().await.with_context(|| "请求失败")?;
    let status = response.status();
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    if !status.is_success() {
        bail!("HTTP {status}");
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(resumed)
        .write(true)
        .truncate(!resumed)
        .open(part)
        .with_context(|| format!("打开临时文件失败: {}", part.display()))?;
    if !resumed {
        // 服务端不支持 Range：从头写。
        offset = 0;
    }
    let mut written = offset;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| "读取响应失败")?;
        written += chunk.len() as u64;
        if written > expected {
            bail!("响应长度超出预期（{expected} 字节）");
        }
        out.write_all(&chunk).with_context(|| "写入临时文件失败")?;
    }
    out.flush().ok();
    if written != expected {
        bail!("连接提前结束（{written}/{expected} 字节）");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("读取文件失败: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read =
            std::io::Read::read(&mut file, &mut buffer).with_context(|| "计算 sha256 失败")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_model::catalog::LocalModelKind;
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    /// 测试用模型：5 个小文件，内容由路径决定。
    fn content(path: &str) -> Vec<u8> {
        path.repeat(700).into_bytes()
    }

    fn leak(value: String) -> &'static str {
        Box::leak(value.into_boxed_str())
    }

    fn test_spec(corrupt_onnx: bool) -> LocalModelSpec {
        let f = |path: &'static str| {
            let bytes = content(path);
            let digest = if corrupt_onnx && path.ends_with(".onnx") {
                "0".repeat(64)
            } else {
                sha(&bytes)
            };
            ModelFile {
                path,
                size: bytes.len() as u64,
                sha256: leak(digest),
            }
        };
        LocalModelSpec {
            id: "test-model",
            kind: LocalModelKind::Embedding,
            repo: "org/test-model",
            revision: "0123456789012345678901234567890123456789",
            onnx: f("onnx/model_quantized.onnx"),
            tokenizer: f("tokenizer.json"),
            config: f("config.json"),
            special_tokens_map: f("special_tokens_map.json"),
            tokenizer_config: f("tokenizer_config.json"),
            dimension: 4,
        }
    }

    /// 极简 HTTP 服务：支持 Range；前 `drop_first` 次请求只发一半内容后断开。
    fn serve(drop_first: usize) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .splitn(5, '/')
                    .nth(4)
                    .unwrap_or("")
                    .to_string();
                let path = path
                    .split_once('/')
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_default();
                let body = content(&path);
                let start = request
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("range: bytes=")
                            .map(str::to_string)
                    })
                    .and_then(|value| value.trim_end_matches('-').parse::<usize>().ok())
                    .unwrap_or(0);
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let slice = &body[start.min(body.len())..];
                let status = if start > 0 {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    slice.len()
                );
                let _ = stream.write_all(header.as_bytes());
                if index < drop_first {
                    let _ = stream.write_all(&slice[..slice.len() / 2]);
                    continue; // 提前断开
                }
                let _ = stream.write_all(slice);
            }
        });
        (base, hits)
    }

    const FAST_RETRY: Duration = Duration::from_millis(10);

    #[tokio::test]
    async fn downloads_resumes_and_verifies() {
        let (base, hits) = serve(2);
        let sources = [base];
        let root = tempfile::tempdir().unwrap();
        let spec = test_spec(false);
        let dir = ensure_model_from(root.path(), &spec, &sources, FAST_RETRY)
            .await
            .expect("下载应成功");
        assert!(is_installed(root.path(), &spec));
        assert_eq!(
            std::fs::read(dir.join("model_quantized.onnx")).unwrap(),
            content("onnx/model_quantized.onnx")
        );
        assert!(!root.path().join(".partial-test-model").exists());
        // 前两次断开，续传后成功：总请求数 = 5 个文件 + 2 次续传。
        assert_eq!(hits.load(Ordering::SeqCst), 7);

        // 已安装时不再请求。
        ensure_model_from(root.path(), &spec, &sources, FAST_RETRY)
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 7);
    }

    #[tokio::test]
    async fn rejects_checksum_mismatch() {
        let (base, _) = serve(0);
        let root = tempfile::tempdir().unwrap();
        let spec = test_spec(true);
        let error = download_file(
            &reqwest::Client::new(),
            std::slice::from_ref(&base),
            &spec,
            &spec.onnx,
            root.path(),
            FAST_RETRY,
        )
        .await
        .expect_err("sha256 不符应失败");
        assert!(format!("{error:#}").contains("校验失败"), "{error:#}");
        assert!(!root.path().join("model_quantized.onnx").exists());
        assert!(!is_installed(root.path(), &spec));
    }

    #[test]
    #[serial_test::serial]
    fn mirror_env_is_tried_first() {
        unsafe { std::env::set_var(MODEL_MIRROR_ENV, " https://a.example/ ,https://b.example") };
        let list = sources();
        unsafe { std::env::remove_var(MODEL_MIRROR_ENV) };
        assert_eq!(
            list,
            [
                "https://a.example",
                "https://b.example",
                "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com/memory-models",
                "https://www.modelscope.cn/models/{repo}/resolve/master/{path}",
                "https://hf-mirror.com",
                "https://huggingface.co",
            ]
        );
        let spec = test_spec(false);
        assert_eq!(
            file_url(&list[0], &spec, &spec.onnx),
            "https://a.example/org/test-model/resolve/0123456789012345678901234567890123456789/onnx/model_quantized.onnx"
        );
        assert_eq!(
            file_url(&list[3], &spec, &spec.onnx),
            "https://www.modelscope.cn/models/org/test-model/resolve/master/onnx/model_quantized.onnx"
        );
        assert_eq!(
            file_url(&list[2], &spec, &spec.tokenizer),
            "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com/memory-models/org/test-model/resolve/0123456789012345678901234567890123456789/tokenizer.json"
        );
    }
}
