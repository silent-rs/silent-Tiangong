//! 平台文件拷贝层：优先写时复制（CoW），回退普通拷贝。
//!
//! - macOS：`clonefile`（APFS 克隆，瞬时且与源文件完全独立）
//! - Linux：`FICLONE` ioctl（btrfs/xfs 等 reflink；ext4 等不支持时自动回退）
//! - 其它平台 / CoW 失败（跨卷、文件系统不支持）：`fs::copy`

use std::fs;
use std::io;
use std::path::Path;

/// 实际使用的拷贝方式（统计与测试用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMethod {
    CloneFile,
    Ficlone,
    FullCopy,
}

/// 拷贝单个文件到目标路径（目标必须不存在，CoW 语义要求）。
pub fn copy_file(src: &Path, dst: &Path) -> io::Result<CopyMethod> {
    #[cfg(target_os = "macos")]
    {
        match clonefile_copy(src, dst) {
            Ok(()) => Ok(CopyMethod::CloneFile),
            Err(clone_err) => {
                // CoW 失败（跨卷 / 文件系统不支持）时回退普通拷贝；
                // 回退也失败才对外报错，报告回退时的错误。
                fs::copy(src, dst)
                    .map(|_| CopyMethod::FullCopy)
                    .inspect_err(|_copy_err| {
                        let _ = clone_err;
                    })
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        match ficlone_copy(src, dst) {
            Ok(()) => return Ok(CopyMethod::Ficlone),
            Err(clone_err) => {
                return fs::copy(src, dst)
                    .map(|_| CopyMethod::FullCopy)
                    .map_err(|copy_err| {
                        let _ = clone_err;
                        copy_err
                    });
            }
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (src, dst);
        fs::copy(src, dst).map(|_| CopyMethod::FullCopy)
    }
}

/// 硬链接（同卷时零开销复用），失败回退拷贝。
///
/// 仅用于快照区内部条目复用：快照区文件创建后绝不原地修改，
/// 因此共享 inode 是安全的；工作区文件必须走 [`copy_file`]。
pub fn link_or_copy(src: &Path, dst: &Path) -> io::Result<CopyMethod> {
    match fs::hard_link(src, dst) {
        Ok(()) => Ok(CopyMethod::CloneFile),
        Err(_) => copy_file(src, dst),
    }
}

#[cfg(target_os = "macos")]
fn clonefile_copy(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let src = CString::new(src.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径包含非法字节"))?;
    let dst = CString::new(dst.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径包含非法字节"))?;
    // SAFETY: 两个路径均已转为合法 C 字符串，flags 传 0 表示无附加选项。
    let ret = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) };
    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn ficlone_copy(src: &Path, dst: &Path) -> io::Result<()> {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    let dst_file = File::create(dst)?;
    let src_file = File::open(src)?;
    // SAFETY: fd 均来自打开的 File，生命周期覆盖调用期间；FICLONE 参数为源 fd。
    let ret = unsafe { libc::ioctl(dst_file.as_raw_fd(), libc::FICLONE, src_file.as_raw_fd()) };
    if ret == 0 {
        // reflink 不保留权限位，与源对齐。
        let perms = src_file.metadata()?.permissions();
        dst_file.set_permissions(perms)?;
        Ok(())
    } else {
        // 文件系统不支持 reflink：交由上层 fs::copy 完整拷贝（目标已截断）。
        Err(io::Error::last_os_error())
    }
}

/// 原子写文件：临时文件 + rename，避免半截 JSON。
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "路径缺少父目录"))?;
    fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    // Windows 上 rename 到已存在目标需要先移除。
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn copy_file_produces_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        fs::write(&src, b"hello snapshot").unwrap();
        let dst = dir.path().join("b.txt");
        let method = copy_file(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"hello snapshot");
        assert_ne!(method, CopyMethod::FullCopy); // 同卷 APFS/btrfs/xfs 下应走 CoW
    }

    #[test]
    fn link_or_copy_dedupes_on_same_volume() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        fs::write(&src, b"link me").unwrap();
        let dst = dir.path().join("b.txt");
        assert_eq!(link_or_copy(&src, &dst).unwrap(), CopyMethod::CloneFile);
        assert_eq!(fs::read(&dst).unwrap(), b"link me");
    }

    #[test]
    fn permissions_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("exec.sh");
        fs::write(&src, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
        let dst = dir.path().join("exec-copy.sh");
        copy_file(&src, &dst).unwrap();
        let mode = fs::metadata(&dst).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}
