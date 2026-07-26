//! Bot 管理所有权锁。
//!
//! 确立「任意时刻只有一个 Bot 管理者」协议（issue #286）：Desktop 与独立 Server
//! 不能同时管理 bot。**Desktop 与 Server 争用同一把独占锁**（`~/.tiangong/bot-manager.lock`），
//! 由 OS 文件锁保证互斥——进程退出/崩溃/被强杀时由 OS 自动释放，无需依赖裸 PID。
//!
//! 锁文件内容写入所有者标识（仅用于展示与提示，真正互斥由 OS 锁保证）：
//! ```json
//! {"owner": "desktop" | "server"}
//! ```
//!
//! 优先级：`Desktop > 独立 Server > CLI`。
//! - Desktop 运行时：持锁，由 Embedded Server + BotRuntime 管理 bot
//! - Desktop 未运行时：独立 Server 持锁管理 bot
//! - CLI 不持锁，只查询当前所有者：Desktop 在运行则拒绝并提示「请在 Desktop 中操作」；
//!   Server 在运行则经 HTTP 操作；均不在则提示「请先启动 Server」

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::io::storage_root;

/// Bot 管理者种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerKind {
    /// Desktop 应用（含其 Embedded Server + BotRuntime）。
    Desktop,
    /// 独立 Server daemon（headless 下管理 bot）。
    Server,
}

/// 锁文件内写入的所有者记录（仅展示用，互斥由 OS 文件锁保证）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OwnerRecord {
    owner: OwnerKind,
}

/// 统一的 Bot 管理权锁文件路径：`~/.tiangong/bot-manager.lock`。
///
/// Desktop 与 Server 都争用此文件，确保任意时刻只有一个管理者。
///
/// 测试可经 `TIANGONG_TEST_LOCK_DIR` 环境变量重定向到临时目录，避免污染真实存储。
fn lock_path() -> PathBuf {
    if let Ok(dir) = std::env::var("TIANGONG_TEST_LOCK_DIR") {
        return PathBuf::from(dir).join("bot-manager.lock");
    }
    storage_root().join("bot-manager.lock")
}

/// 独占文件锁句柄，持有期间代表当前进程拥有 Bot 管理权。
///
/// 通过 `fs4::FileExt::try_lock_exclusive` 非阻塞获取同一把锁；**获取成功后写入
/// 当前所有者标识**到文件（供 `current_owner` 读取）。锁竞争时立即失败并返回
/// 现有占用方（从文件内容读取，而非猜测对端）。
///
/// 不实现显式 Drop 释放——依赖 OS 在文件句柄关闭（进程退出/崩溃/强杀）时释放独占锁。
/// 句柄存于持有者的生命周期对象（如 `TiangongApp` 字段）中，随其一起 drop。
#[derive(Debug)]
pub struct OwnershipLock {
    kind: OwnerKind,
    // 持有打开的锁文件句柄；只要本对象存活，独占锁就保持。drop 时文件关闭→锁释放。
    _file: File,
}

impl OwnershipLock {
    /// 尝试获取 Bot 管理独占锁。
    ///
    /// Desktop 与 Server 都争用同一把锁（`bot-manager.lock`）。成功返回锁句柄
    /// （调用方持有至退出，文件内已写入本类所有者标识）；失败返回现有占用方
    /// （从锁文件内容读取，准确反映是 Desktop 还是 Server）。IO 错误返回 `Err`。
    pub fn acquire(kind: OwnerKind) -> Result<std::result::Result<OwnershipLock, OwnerKind>> {
        let path = lock_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建锁文件目录失败: {}", parent.display()))?;
        }
        // 以读写方式打开：获取锁后需要写入所有者标识；竞争时只读现有内容。
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("打开锁文件失败: {}", path.display()))?;
        // 非阻塞独占锁：已被占用（Desktop 或 Server 任一）则返回错误。
        match fs4::FileExt::try_lock_exclusive(&file) {
            Ok(()) => {
                // 获取成功：写入当前所有者标识（仅展示用），truncate 旧内容。
                write_owner(&file, kind)?;
                Ok(Ok(OwnershipLock { kind, _file: file }))
            }
            Err(_) => {
                // 锁被占用：从文件内容读取现有所有者，准确报告（而非猜测对端）。
                let holder = read_owner(&file).unwrap_or(None);
                drop(file);
                Ok(Err(holder.unwrap_or(kind)))
            }
        }
    }

    /// 当前 Bot 管理者（不获取锁，仅探测，读锁文件内容）。
    ///
    /// 用于 CLI 在执行 bot 命令前判断应拒绝（Desktop）还是走 HTTP（Server）。
    /// 返回 `None` 表示无管理者（锁未被持有或文件无内容）。
    pub fn current_owner() -> Option<OwnerKind> {
        let path = lock_path();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .ok()?;
        // 锁未被持有 → 无管理者。
        if fs4::FileExt::try_lock_exclusive(&file).is_ok() {
            let _ = fs4::FileExt::unlock(&file);
            return None;
        }
        // 锁被持有：读文件内容判断所有者。
        read_owner(&file).unwrap_or(None)
    }

    /// 本句柄代表的所有者种类。
    pub fn kind(&self) -> OwnerKind {
        self.kind
    }
}

/// 向锁文件写入所有者标识（获取锁成功后调用）。
fn write_owner(file: &File, kind: OwnerKind) -> Result<()> {
    use std::io::Seek;
    let record = OwnerRecord { owner: kind };
    let json = serde_json::to_string(&record).context("序列化所有者标识失败")?;
    let mut f = file;
    f.seek(std::io::SeekFrom::Start(0)).ok();
    f.set_len(0).ok();
    f.write_all(json.as_bytes()).context("写入所有者标识失败")?;
    f.flush().ok();
    Ok(())
}

/// 从锁文件读取所有者标识（竞争失败或探测时调用）。文件无有效内容返回 Ok(None)。
fn read_owner(file: &File) -> Result<Option<OwnerKind>> {
    use std::io::{Read, Seek};
    let mut f = file;
    let _ = f.seek(std::io::SeekFrom::Start(0));
    let mut buf = String::new();
    f.read_to_string(&mut buf).context("读取所有者标识失败")?;
    if buf.trim().is_empty() {
        return Ok(None);
    }
    let record: OwnerRecord = serde_json::from_str(&buf).context("解析所有者标识失败")?;
    Ok(Some(record.owner))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在临时目录构造 OwnershipLock 测试——通过临时锁文件路径绕开全局 storage_root。
    /// 这些测试直接测 fs4 互斥语义 + OwnerRecord 序列化。

    /// 同一锁文件第二次获取失败，释放后可再次获取。
    #[test]
    fn single_lock_is_exclusive_then_releasable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bot-manager.lock");
        let f1 = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(fs4::FileExt::try_lock_exclusive(&f1).is_ok());
        let f2 = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(
            fs4::FileExt::try_lock_exclusive(&f2).is_err(),
            "同一锁文件第二次获取应失败"
        );
        fs4::FileExt::unlock(&f1).unwrap();
        drop(f1);
        assert!(
            fs4::FileExt::try_lock_exclusive(&f2).is_ok(),
            "释放后应可再次获取"
        );
    }

    /// 所有者标识序列化/反序列化往返正确。
    #[test]
    fn owner_record_roundtrip() {
        for kind in [OwnerKind::Desktop, OwnerKind::Server] {
            let record = OwnerRecord { owner: kind };
            let json = serde_json::to_string(&record).unwrap();
            let parsed: OwnerRecord = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.owner, kind);
        }
        // snake_case 序列化。
        assert_eq!(
            serde_json::to_string(&OwnerRecord {
                owner: OwnerKind::Desktop
            })
            .unwrap(),
            r#"{"owner":"desktop"}"#
        );
        assert_eq!(
            serde_json::to_string(&OwnerRecord {
                owner: OwnerKind::Server
            })
            .unwrap(),
            r#"{"owner":"server"}"#
        );
    }

    /// 写入所有者后可读回正确种类；空文件返回 None。
    #[test]
    fn write_and_read_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("owner.lock");
        let f = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        write_owner(&f, OwnerKind::Server).unwrap();
        let read = read_owner(&f).unwrap();
        assert_eq!(read, Some(OwnerKind::Server));

        // 空文件返回 None。
        use std::io::Seek;
        let mut f2 = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        f2.set_len(0).ok();
        let _ = f2.seek(std::io::SeekFrom::Start(0));
        assert_eq!(read_owner(&f2).unwrap(), None);
    }
}
