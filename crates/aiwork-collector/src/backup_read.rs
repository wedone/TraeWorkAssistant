//! 读取被其它进程独占锁定的文件（Windows 备份语义）。
//!
//! Trae CN 运行时会以独占方式打开 `Network\Cookies`，普通 `std::fs::read` 会报
//! `os error 32`。这里在普通读取失败后，启用 `SeBackupPrivilege` 并用
//! `FILE_FLAG_BACKUP_SEMANTICS` 打开文件——这是备份软件读取锁定文件的标准做法。
//!
//! 该能力需要管理员权限（UAC 提升）；普通权限会返回明确的“需要管理员”错误。

use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE, LUID,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_ACCESS_DENIED: i32 = 5;

/// 读取文件：普通读取优先；共享冲突/拒绝访问时回退到备份语义读取。
pub fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION) | Some(ERROR_ACCESS_DENIED)
            ) =>
        {
            read_backup(path)
        }
        Err(e) => Err(format!("读取失败: {e}")),
    }
}

fn read_backup(path: &Path) -> Result<Vec<u8>, String> {
    enable_backup_privilege()?;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let err = unsafe { GetLastError() };
        return Err(format!(
            "备份方式打开失败（错误 {err}）；请右键本程序选择「以管理员身份运行」"
        ));
    }

    // 用 std::fs::File 接管句柄：File 析构自动 CloseHandle，避免手写 ReadFile 循环
    let mut file = unsafe { std::fs::File::from_raw_handle(handle as _) };
    let mut out = Vec::new();
    file.read_to_end(&mut out)
        .map_err(|e| format!("备份方式读取失败: {e}"))?;
    Ok(out)
}

/// 启用当前进程的 SeBackupPrivilege；普通权限返回“需要管理员”
fn enable_backup_privilege() -> Result<(), String> {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        let ok = OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        );
        if ok == 0 {
            return Err("需要以管理员身份运行才能读取被 Trae 锁定的 Cookies".into());
        }

        let name: Vec<u16> = "SeBackupPrivilege\0".encode_utf16().collect();
        let mut luid: LUID = std::mem::zeroed();
        if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) == 0 {
            let err = GetLastError();
            CloseHandle(token);
            return Err(format!("查找 SeBackupPrivilege 失败（错误 {err}）"));
        }

        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let ok = AdjustTokenPrivileges(
            token,
            0,
            &tp,
            std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        let last = GetLastError();
        CloseHandle(token);
        if ok == 0 || last == ERROR_NOT_ALL_ASSIGNED {
            return Err("需要以管理员身份运行才能读取被 Trae 锁定的 Cookies".into());
        }
        Ok(())
    }
}
