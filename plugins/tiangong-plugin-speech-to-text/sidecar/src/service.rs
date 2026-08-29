//! Speech-To-Text sidecar 业务服务。
//!
//! 校验音频路径 → 读取音频 → 解析 STT 端点 → 调供应商 → 返回转写文本。

use anyhow::{Context, Result};
use tiangong_llm::ModelCapability;
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};
use tiangong_plugin_runtime::sidecar::STORAGE_ROOT_ENV;
use tiangong_plugin_speech_to_text_protocol::{
    Empty, PLUGIN_ID, PLUGIN_VERSION, RECORD_CANCEL_OPERATION, RECORD_START_OPERATION,
    RECORD_STOP_OPERATION, RecordControlRequest, RecordStartRequest, RecordStartResponse,
    RecordStopResponse, STT_PROTOCOL_VERSION, TRANSCRIBE_OPERATION, TranscribeRequest,
    TranscribeResponse,
};

pub struct SttService;

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for SttService {
    async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "STT 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                    request.protocol_version
                ),
                false,
            );
        }

        let payload = match dispatch_operation(&request.operation, request.payload).await {
            Ok(value) => value,
            Err(error) => {
                return Response::error(
                    &request_id,
                    ErrorCode::ServiceError,
                    error.to_string(),
                    false,
                );
            }
        };
        Response::success(&request_id, payload)
    }
}

async fn dispatch_operation(
    operation: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    match operation {
        HANDSHAKE_OPERATION => serde_json::to_value(HandshakeResponse {
            plugin_id: PLUGIN_ID.to_string(),
            plugin_version: PLUGIN_VERSION.to_string(),
            sidecar_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            business_protocol: STT_PROTOCOL_VERSION,
            capabilities: vec!["speech_to_text".to_string()],
            instance_id: format!("stt-sidecar-{}", std::process::id()),
            status: ServiceStatus::Ready,
        })
        .context("序列化 STT 握手响应失败"),

        TRANSCRIBE_OPERATION => {
            let req: TranscribeRequest =
                serde_json::from_value(payload).context("解析 transcribe 请求失败")?;
            let result = transcribe(req).await?;
            serde_json::to_value(result).context("序列化 transcribe 响应失败")
        }

        RECORD_START_OPERATION => {
            let req: RecordStartRequest =
                serde_json::from_value(payload).context("解析 record_start 请求失败")?;
            let result = record_start(req)?;
            serde_json::to_value(result).context("序列化 record_start 响应失败")
        }

        RECORD_STOP_OPERATION => {
            let req: RecordControlRequest =
                serde_json::from_value(payload).context("解析 record_stop 请求失败")?;
            let result = record_stop(req)?;
            serde_json::to_value(result).context("序列化 record_stop 响应失败")
        }

        RECORD_CANCEL_OPERATION => {
            let req: RecordControlRequest =
                serde_json::from_value(payload).context("解析 record_cancel 请求失败")?;
            record_cancel(req)?;
            serde_json::to_value(Empty {}).context("序列化 record_cancel 响应失败")
        }

        other => Err(anyhow::anyhow!("未知的 STT 操作: {other}")),
    }
}

/// 转写音频：校验路径 → 读取音频 → 调 STT 供应商 → 返回文本。
async fn transcribe(req: TranscribeRequest) -> Result<TranscribeResponse> {
    if req.file_path.trim().is_empty() {
        anyhow::bail!("file_path 不能为空");
    }

    // 安全限制：仅允许读取 ~/.tiangong/media/ 目录内的音频文件。
    let storage_root = std::env::var(STORAGE_ROOT_ENV)
        .context("TIANGONG_STORAGE_ROOT 未注入，无法定位媒体目录")?;
    let media_dir = std::path::PathBuf::from(&storage_root).join("media");
    let mime_type = match std::path::Path::new(&req.file_path)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") | Some("oga") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("webm") => "audio/webm",
        Some("m4a") => "audio/mp4",
        _ => anyhow::bail!("不支持的音频格式（仅支持 mp3/wav/ogg/flac/webm/m4a）"),
    };

    let canonical = std::fs::canonicalize(&req.file_path).context("文件不存在或无法访问")?;
    let canonical_media = std::fs::canonicalize(&media_dir).context("媒体目录不存在")?;
    if !canonical.starts_with(&canonical_media) {
        anyhow::bail!("音频文件必须在 ~/.tiangong/media 目录下");
    }

    let audio_data = std::fs::read(&canonical).context("读取音频文件失败")?;

    // 解析 STT 端点。
    let resolved = tiangong_plugin_sidecar::model::resolve_for_capability(ModelCapability::Stt)?;
    let output = tiangong_core::media::transcribe_audio_with(
        &resolved,
        audio_data,
        mime_type.to_string(),
        req.language,
    )
    .await
    .map_err(|e| anyhow::anyhow!("STT 供应商调用失败：{e}"))?;

    Ok(TranscribeResponse {
        text: output.response.text,
        language: output.response.language,
        duration: output.response.duration,
        model: output.resolved.model,
        audio_path: req.file_path,
    })
}

// ── 录音（record_start / record_stop / record_cancel）──
//
// 用系统录音命令采集麦克风。录音会话（进程 + 文件路径 + 采样率）由全局
// 静态变量管理（sidecar 单进程、同一时刻至多一路录音）。跨平台：
// - macOS / Windows：ffmpeg（需用户自行安装，启动前检测并给出明确报错）
// - Linux：arecord（ALSA 默认）
//
// 停止录音走「优雅终止」：录音进程被强杀（SIGKILL/TerminateProcess）时
// WAV 头部的长度字段来不及回写，短录音会留下 0 字节或头部损坏的文件。
// Unix 下发 SIGINT 让 ffmpeg/arecord 自行收尾；收尾后统一校验并重写 WAV
// 头部长度，兜底任何残留的占位值。

use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

/// 一次录音会话：录音进程 + 本次的输出文件、采样率与会话 ID。
struct RecordSession {
    child: std::process::Child,
    file_path: std::path::PathBuf,
    sample_rate: u32,
    /// 会话 ID：停止/取消请求必须携带且匹配，防止迟到的旧请求误杀新录音。
    session_id: String,
}

static RECORDING: Mutex<Option<RecordSession>> = Mutex::new(None);

/// Windows 下抑制子进程控制台窗口（CREATE_NO_WINDOW），避免录音时闪黑窗。
#[cfg(windows)]
fn suppress_console_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn suppress_console_window(_command: &mut std::process::Command) {}

/// 检测录音命令是否可用（ffmpeg/arecord 未安装时给出可操作的报错）。
fn ensure_recording_tool_available() -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    let (program, version_flag, hint) = (
        "ffmpeg",
        "-version",
        "brew install ffmpeg（或 choco install ffmpeg）",
    );
    #[cfg(target_os = "linux")]
    let (program, version_flag, hint) = (
        "arecord",
        "--version",
        "安装 alsa-utils（如 sudo apt install alsa-utils）",
    );

    let mut probe = std::process::Command::new(program);
    probe.arg(version_flag);
    suppress_console_window(&mut probe);
    let available = probe
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .is_ok();
    if !available {
        anyhow::bail!("录音功能需要 {program}，当前系统未安装。请先安装：{hint}");
    }
    Ok(())
}

/// 开始录音：使用调用方传入的会话 ID，启动系统录音命令。
fn record_start(req: RecordStartRequest) -> Result<RecordStartResponse> {
    let session_id = req.session_id.trim().to_string();
    if session_id.is_empty() {
        anyhow::bail!("record_start 缺少 session_id（由调用方生成并传入）");
    }

    let mut guard = RECORDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(session) = guard.as_mut() {
        // 上一次录音进程可能已自行退出（如设备被占用），先收割再判定占用。
        if session
            .child
            .try_wait()
            .is_ok_and(|status| status.is_none())
        {
            anyhow::bail!("已有录音在进行中");
        }
        *guard = None;
    }

    ensure_recording_tool_available()?;

    let sample_rate = req.sample_rate.unwrap_or(16000);
    let file_path = media_file_path("stt_rec", "wav")?;

    let mut command = recording_command(sample_rate, &file_path);
    suppress_console_window(&mut command);
    let child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("启动录音失败：{e}"))?;

    *guard = Some(RecordSession {
        child,
        file_path,
        sample_rate,
        session_id: session_id.clone(),
    });
    Ok(RecordStartResponse { session_id })
}

/// 按平台构造录音命令（16bit 单声道 PCM WAV）。
fn recording_command(sample_rate: u32, file_path: &std::path::Path) -> std::process::Command {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("ffmpeg");
        command.args([
            "-y",
            "-f",
            "avfoundation",
            "-i",
            ":0",
            "-ar",
            &sample_rate.to_string(),
            "-ac",
            "1",
        ]);
        command.arg(file_path);
        command
    };

    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("ffmpeg");
        command.args([
            "-y",
            "-f",
            "dshow",
            "-i",
            "audio=default",
            "-ar",
            &sample_rate.to_string(),
            "-ac",
            "1",
        ]);
        command.arg(file_path);
        command
    };

    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = std::process::Command::new("arecord");
        command.args(["-f", "S16_LE", "-r", &sample_rate.to_string(), "-c", "1"]);
        command.arg(file_path);
        command
    };

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let mut command = {
        let _ = (sample_rate, file_path);
        std::process::Command::new("")
    };

    let _ = &mut command;
    command
}

/// 优雅终止录音进程：Unix 发 SIGINT 让 ffmpeg/arecord 自行写完 WAV 头，
/// 等待至多 3 秒；超时或 Windows 下退回强杀（由 WAV 头修复兜底）。
fn stop_recording_child(session: &mut RecordSession) {
    #[cfg(unix)]
    {
        // std::process::Child 只有强杀（kill = SIGKILL），SIGINT 需经 libc 发送。
        let pid = session.child.id() as i32;
        unsafe {
            libc::kill(pid, libc::SIGINT);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if session
                .child
                .try_wait()
                .is_ok_and(|status| status.is_some())
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    let _ = session.child.kill();
    let _ = session.child.wait();
}

/// 停止录音：会话匹配校验 → 优雅终止进程 → 校验/修复 WAV 头 → 返回文件路径与时长。
fn record_stop(req: RecordControlRequest) -> Result<RecordStopResponse> {
    let mut guard = RECORDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut session = take_matching_session(&mut guard, &req)?;

    stop_recording_child(&mut session);

    finalize_wav_header(&session.file_path)?;
    let duration = std::fs::metadata(&session.file_path)
        .ok()
        .map(|m| (m.len().saturating_sub(44)) as f64 / (session.sample_rate as f64 * 2.0));

    Ok(RecordStopResponse {
        file_path: session.file_path.to_string_lossy().to_string(),
        mime_type: "audio/wav".to_string(),
        duration,
    })
}

/// 取消录音：会话匹配校验 → 终止进程并删除录音文件（用户放弃，不保留产物）。
///
/// 没有录音或会话不匹配时静默成功：取消是幂等操作，迟到的旧取消请求
/// 不应向用户报错。
fn record_cancel(req: RecordControlRequest) -> Result<()> {
    let mut guard = RECORDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut session) = take_matching_session(&mut guard, &req) {
        stop_recording_child(&mut session);
        let _ = std::fs::remove_file(&session.file_path);
    }
    Ok(())
}

/// 取出与请求会话 ID 匹配的录音会话；不匹配时**不动现有录音**并返回错误。
///
/// 防竞态核心：迟到的旧取消/停止请求（session_id 是旧录音的）不能终止
/// 已经开始的新录音——会话 ID 不匹配直接报错，RECORDING 槽位保持原样。
fn take_matching_session(
    guard: &mut Option<RecordSession>,
    req: &RecordControlRequest,
) -> Result<RecordSession> {
    let session = guard
        .take()
        .ok_or_else(|| anyhow::anyhow!("当前没有录音在进行中"))?;
    if session.session_id != req.session_id {
        // 放回新录音，拒绝旧会话的请求。
        *guard = Some(session);
        anyhow::bail!(
            "录音会话不匹配（请求 {}，当前为其他会话），已忽略",
            req.session_id
        );
    }
    Ok(session)
}

/// 校验并重写 WAV 头部长度字段。
///
/// 录音进程被强杀（或未正常收尾）时，RIFF/data 长度为占位值；本函数按
/// 文件实际大小重写两处长度的 little-endian 值。文件不存在、过短（不足
/// 一个 44 字节标准头）或非 RIFF/WAVE 时明确报错或跳过。
fn finalize_wav_header(path: &std::path::Path) -> Result<()> {
    let file_len = std::fs::metadata(path)
        .with_context(|| format!("录音文件不存在：{}", path.display()))?
        .len();
    if file_len < 44 {
        anyhow::bail!("录音文件只有 {file_len} 字节，录音可能失败（请检查麦克风权限与录音设备）");
    }

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("打开录音文件失败：{}", path.display()))?;
    let mut header = [0u8; 44];
    file.read_exact(&mut header).context("读取 WAV 头失败")?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        // 非 WAV 容器（fmt chunk 非标准布局）：无法安全重写，保留原样。
        return Ok(());
    }

    let riff_size = u32::try_from(file_len.saturating_sub(8)).unwrap_or(u32::MAX);
    header[4..8].copy_from_slice(&riff_size.to_le_bytes());
    // 解析 fmt chunk 实际大小后定位 data 长度字段（PCM 通常为 44 字节整头）。
    let fmt_size = u32::from_le_bytes([header[16], header[17], header[18], header[19]]) as usize;
    let data_size_offset = 20 + fmt_size + 4;
    if data_size_offset + 4 <= header.len() {
        let data_size =
            u32::try_from(file_len.saturating_sub(data_size_offset as u64 + 4)).unwrap_or(u32::MAX);
        header[data_size_offset..data_size_offset + 4].copy_from_slice(&data_size.to_le_bytes());
    }

    file.seek(SeekFrom::Start(0)).context("回卷文件失败")?;
    file.write_all(&header).context("重写 WAV 头失败")?;
    file.flush().context("落盘 WAV 头失败")?;
    Ok(())
}

/// 构造 `~/.tiangong/media/<prefix>_<scru128>.<ext>` 路径并确保目录存在。
fn media_file_path(prefix: &str, ext: &str) -> Result<std::path::PathBuf> {
    let storage_root = std::env::var(STORAGE_ROOT_ENV)
        .context("TIANGONG_STORAGE_ROOT 未注入，无法定位媒体目录")?;
    let dir = std::path::PathBuf::from(&storage_root).join("media");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建媒体目录失败：{}", dir.display()))?;
    let file_name = format!("{}_{}.{}", prefix, scru128::new(), ext);
    Ok(dir.join(file_name))
}
