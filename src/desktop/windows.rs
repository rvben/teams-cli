//! Windows facts for listener verification, read directly from the OS: the TCP listener
//! table, one process snapshot, the owning package and user of the Teams process, and the
//! user-wide WebView2 arguments.
use super::owner::{Listener, System};
use crate::error::AppError;
use std::collections::HashMap;
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, INVALID_HANDLE_VALUE},
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
    },
    Networking::WinSock::{AF_INET, AF_INET6},
    Security::{GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
    Storage::Packaging::Appx::{GetPackageFamilyName, PACKAGE_FAMILY_NAME_MAX_LENGTH},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        Registry::{
            HKEY_CURRENT_USER, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
        },
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

/// The Microsoft Store package family of new Teams.
const TEAMS_FAMILY: &str = "MSTeams_8wekyb3d8bbwe";

pub struct WindowsSystem;

/// Closes the wrapped handle on drop.
struct Handle(HANDLE);

impl Handle {
    fn new(handle: HANDLE) -> Option<Self> {
        (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(Self(handle))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned open by the OS and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

impl System for WindowsSystem {
    fn listeners(&self, port: u16) -> Result<Vec<Listener>, AppError> {
        let mut listeners = vec![];
        for row in tcp_table::<MIB_TCPROW_OWNER_PID>(AF_INET)? {
            if u16::from_be(row.dwLocalPort as u16) == port {
                listeners.push(Listener {
                    pid: row.dwOwningPid,
                    // dwLocalAddr is in network byte order.
                    loopback: row.dwLocalAddr.to_ne_bytes() == [127, 0, 0, 1],
                });
            }
        }
        for row in tcp_table::<MIB_TCP6ROW_OWNER_PID>(AF_INET6)? {
            if u16::from_be(row.dwLocalPort as u16) == port {
                listeners.push(Listener {
                    pid: row.dwOwningPid,
                    loopback: row.ucLocalAddr == std::net::Ipv6Addr::LOCALHOST.octets(),
                });
            }
        }
        Ok(listeners)
    }

    fn processes(&self) -> Result<HashMap<u32, (u32, String)>, AppError> {
        let failed = || AppError::Desktop("could not inspect running processes".into());
        // SAFETY: plain snapshot of the process list; the handle is closed by `Handle`.
        let snapshot = Handle::new(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })
            .ok_or_else(failed)?;
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut processes = HashMap::new();
        // SAFETY: `entry` is a correctly sized PROCESSENTRY32W owned by this frame.
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
        while more {
            let name = &entry.szExeFile;
            let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            processes.insert(
                entry.th32ProcessID,
                (
                    entry.th32ParentProcessID,
                    String::from_utf16_lossy(&name[..end]),
                ),
            );
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }
        Ok(processes)
    }

    fn trusted_teams(&self, pid: u32) -> bool {
        // SAFETY: query-only access; the handle is closed by `Handle`.
        let Some(process) =
            Handle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })
        else {
            return false;
        };
        package_family(&process).as_deref() == Some(TEAMS_FAMILY)
            && user_sid(process.0).is_some_and(|sid| {
                // SAFETY: the pseudo handle of the current process needs no closing.
                user_sid(unsafe { GetCurrentProcess() }) == Some(sid)
            })
    }

    fn user_debugging_flags(&self) -> bool {
        user_environment("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").is_some_and(|value| {
            value.split_whitespace().any(|token| {
                ["--remote-debugging-port", "--remote-debugging-address"]
                    .iter()
                    .any(|flag| token == *flag || token.starts_with(&format!("{flag}=")))
            })
        })
    }
}

/// A string value of the user-scope environment, as stored (not expanded).
fn user_environment(name: &str) -> Option<String> {
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (key, value) = (wide("Environment"), wide(name));
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut size = 0u32;
    // SAFETY: a size query with a null buffer; the strings are NUL-terminated.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: the buffer holds `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            flags,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    buffer.truncate((size as usize / 2).min(buffer.len()));
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// Every row of the listener table for one address family.
fn tcp_table<Row: Copy>(family: u16) -> Result<Vec<Row>, AppError> {
    let failed = || AppError::Desktop("could not inspect TCP listeners".into());
    let mut size = 0u32;
    let mut buffer: Vec<u64> = vec![];
    for _ in 0..4 {
        // SAFETY: the buffer holds `size` bytes, 8-byte aligned for the table header.
        let status = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut size,
                0,
                family.into(),
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if status == 0 {
            // The table is a u32 row count followed by the rows, aligned for `Row`.
            let base = buffer.as_ptr().cast::<u8>();
            // SAFETY: on success the buffer starts with the row count.
            let count = unsafe { base.cast::<u32>().read() } as usize;
            let offset = size_of::<u32>().next_multiple_of(align_of::<Row>());
            if offset + count * size_of::<Row>() > size as usize {
                return Err(failed());
            }
            // SAFETY: bounds were checked against the size the OS reported.
            let rows = unsafe { base.add(offset).cast::<Row>() };
            return Ok((0..count)
                // SAFETY: in bounds, as checked above.
                .map(|i| unsafe { rows.add(i).read_unaligned() })
                .collect());
        }
        if status != ERROR_INSUFFICIENT_BUFFER {
            return Err(failed());
        }
        buffer = vec![0; (size as usize).div_ceil(8)];
    }
    Err(failed())
}

fn package_family(process: &Handle) -> Option<String> {
    let mut length = PACKAGE_FAMILY_NAME_MAX_LENGTH + 1;
    let mut name = vec![0u16; length as usize];
    // SAFETY: `name` holds `length` UTF-16 units.
    let status = unsafe { GetPackageFamilyName(process.0, &mut length, name.as_mut_ptr()) };
    if status != 0 {
        return None;
    }
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Some(String::from_utf16_lossy(&name[..end]))
}

/// The raw SID of the process token's user.
fn user_sid(process: HANDLE) -> Option<Vec<u8>> {
    let mut token = std::ptr::null_mut();
    // SAFETY: query-only token access; the handle is closed by `Handle`.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = Handle::new(token)?;
    let mut size = 0u32;
    // SAFETY: a size query with no buffer.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut size) };
    let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: the buffer holds `size` bytes, aligned for TOKEN_USER.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    };
    if ok == 0 {
        return None;
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points into it.
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let sid = user.User.Sid;
        let length = GetLengthSid(sid) as usize;
        Some(std::slice::from_raw_parts(sid.cast::<u8>(), length).to_vec())
    }
}
