use std::io;
use std::path::{Path, PathBuf};

/// 解析真实路径；Windows AppContainer 无法查询标准规范化名称时，
/// 改用已打开句柄返回的最终路径，避免要求访问工作区的祖先目录。
pub fn canonicalize_path(path: &Path) -> io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        #[cfg(windows)]
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            windows::canonicalize_opened_path(path).or(Err(error))
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_NAME_OPENED, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetFinalPathNameByHandleW, OPEN_EXISTING,
        VOLUME_NAME_DOS,
    };

    use super::*;

    pub(super) fn canonicalize_opened_path(path: &Path) -> io::Result<PathBuf> {
        let path_wide = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let handle = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let flags = FILE_NAME_OPENED | VOLUME_NAME_DOS;
        let needed = unsafe {
            GetFinalPathNameByHandleW(handle.as_raw_handle(), std::ptr::null_mut(), 0, flags)
        };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u16; needed as usize];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                handle.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                flags,
            )
        };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length as usize >= buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows 最终路径长度在读取期间发生变化",
            ));
        }
        buffer.truncate(length as usize);
        Ok(PathBuf::from(OsString::from_wide(&buffer)))
    }
}
