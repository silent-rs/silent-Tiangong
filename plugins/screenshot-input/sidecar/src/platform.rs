use std::path::Path;
use std::process::{Command, Output};

pub(crate) enum CaptureOutcome {
    Captured,
    Cancelled,
}

fn outcome_from_output(output: Output, path: &Path, tool: &str) -> Result<CaptureOutcome, String> {
    if path.is_file() {
        return Ok(CaptureOutcome::Captured);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success()
        || stderr.is_empty()
        || stderr.to_ascii_lowercase().contains("cancel")
    {
        return Ok(CaptureOutcome::Cancelled);
    }
    Err(format!("{tool} 截图失败: {stderr}"))
}

#[cfg(target_os = "macos")]
pub(crate) fn capture_to_file(path: &Path) -> Result<CaptureOutcome, String> {
    let output = Command::new("/usr/sbin/screencapture")
        .args(["-i", "-x", "-t", "png"])
        .arg(path)
        .output()
        .map_err(|error| format!("启动 macOS 区域截图失败，请检查屏幕录制权限: {error}"))?;
    outcome_from_output(output, path, "macOS screencapture")
}

#[cfg(target_os = "linux")]
pub(crate) fn capture_to_file(path: &Path) -> Result<CaptureOutcome, String> {
    let mut failures = Vec::new();
    if command_available("slurp") && command_available("grim") {
        let selection = Command::new("slurp")
            .output()
            .map_err(|error| format!("启动 slurp 区域选择失败: {error}"))?;
        let geometry = String::from_utf8_lossy(&selection.stdout)
            .trim()
            .to_string();
        if !selection.status.success() || geometry.is_empty() {
            let stderr = String::from_utf8_lossy(&selection.stderr)
                .trim()
                .to_string();
            if stderr.is_empty() || stderr.to_ascii_lowercase().contains("cancel") {
                return Ok(CaptureOutcome::Cancelled);
            }
            failures.push(format!("slurp 区域选择失败: {stderr}"));
        } else {
            let output = Command::new("grim")
                .args(["-g", &geometry])
                .arg(path)
                .output()
                .map_err(|error| format!("启动 grim 截图失败: {error}"))?;
            match outcome_from_output(output, path, "grim") {
                Ok(outcome) => return Ok(outcome),
                Err(error) => failures.push(error),
            }
        }
    }

    let candidates: &[(&str, &[&str])] = &[
        ("gnome-screenshot", &["-a", "-f"]),
        (
            "spectacle",
            &["--region", "--background", "--nonotify", "--output"],
        ),
        ("xfce4-screenshooter", &["--region", "--save"]),
        ("maim", &["--select"]),
        ("scrot", &["--select"]),
        ("import", &[]),
    ];
    for (program, args) in candidates {
        if !command_available(program) {
            continue;
        }
        let output = Command::new(program)
            .args(*args)
            .arg(path)
            .output()
            .map_err(|error| format!("启动 {program} 截图失败: {error}"))?;
        match outcome_from_output(output, path, program) {
            Ok(outcome) => return Ok(outcome),
            Err(error) => failures.push(error),
        }
    }
    if !failures.is_empty() {
        return Err(format!("Linux 区域截图失败: {}", failures.join("; ")));
    }

    Err(
        "未找到 Linux 区域截图工具，请安装 grim+slurp、gnome-screenshot、spectacle、xfce4-screenshooter、maim、scrot 或 ImageMagick"
            .to_string(),
    )
}

#[cfg(target_os = "linux")]
fn command_available(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(program).is_file())
    })
}

#[cfg(target_os = "windows")]
pub(crate) fn capture_to_file(path: &Path) -> Result<CaptureOutcome, String> {
    let script_path = path.with_extension("ps1");
    std::fs::write(&script_path, include_str!("capture_windows.ps1")).map_err(|error| {
        format!(
            "写入 Windows 截图脚本失败: {}: {error}",
            script_path.display()
        )
    })?;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-STA", "-File"])
        .arg(&script_path)
        .arg("-OutputPath")
        .arg(path)
        .output()
        .map_err(|error| {
            format!("启动 Windows 区域截图失败，请确认 Windows PowerShell 可用: {error}")
        })?;
    outcome_from_output(output, path, "Windows 区域截图")
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub(crate) fn capture_to_file(_path: &Path) -> Result<CaptureOutcome, String> {
    Err("当前平台不支持交互式区域截图".to_string())
}
