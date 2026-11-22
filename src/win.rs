//! Every Win32 call, so the unsafe code stays in one place.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LocalFree, RECT,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, CreateEventW, EVENT_MODIFY_STATE, GetCurrentThreadId, OpenEventW,
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, SetEvent,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    ASSOCF_NONE, ASSOCSTR_EXECUTABLE, AssocQueryStringW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow,
    GetWindowLongW, GetWindowPlacement, GetWindowThreadProcessId, IsHungAppWindow, IsIconic,
    IsWindowVisible, MSG, PM_NOREMOVE, PeekMessageW, SW_RESTORE, SW_SHOWNORMAL,
    SetForegroundWindow, ShowWindowAsync, WINDOWPLACEMENT, WS_EX_TOOLWINDOW,
};
use windows_sys::core::BOOL;

const STOP_EVENT: &str = r"Local\t3-dock-stop";

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn blob(data: &[u8]) -> Option<CRYPT_INTEGER_BLOB> {
    Some(CRYPT_INTEGER_BLOB {
        cbData: data.len().try_into().ok()?,
        pbData: data.as_ptr().cast_mut(),
    })
}

/// SAFETY: `output` must come from a successful DPAPI call and not be used again.
unsafe fn take(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        bytes
    }
}

/// Encrypts for the current Windows user.
pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
    let input = blob(data)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: the blobs are valid for the call, and the optional arguments may be null.
    unsafe {
        let ok = CryptProtectData(
            &input,
            null(),
            null(),
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );
        (ok != 0).then(|| take(output))
    }
}

pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    let input = blob(data)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    unsafe {
        let ok = CryptUnprotectData(
            &input,
            null_mut(),
            null(),
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );
        (ok != 0).then(|| take(output))
    }
}

/// A named event that both marks the running instance and asks it to stop.
pub struct Instance(HANDLE);

impl Instance {
    /// `None` when another instance runs, or the event cannot be created.
    pub fn claim() -> Option<Self> {
        let name = wide(STOP_EVENT);
        // SAFETY: the name is NUL-terminated and the handle is owned by `Instance`.
        unsafe {
            let handle = CreateEventW(null(), 1, 0, name.as_ptr());
            if handle.is_null() {
                return None;
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return None;
            }
            Some(Self(handle))
        }
    }

    pub fn stop_requested(&self) -> bool {
        // SAFETY: the handle is valid until drop, and a zero timeout does not block.
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Signals the running instance. False when there is none.
pub fn signal_stop() -> bool {
    let name = wide(STOP_EVENT);
    // SAFETY: the name is NUL-terminated and the handle is closed here.
    unsafe {
        let handle = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if handle.is_null() {
            return false;
        }
        let signaled = SetEvent(handle) != 0;
        CloseHandle(handle);
        signaled
    }
}

/// The executable registered for `t3code://`.
pub fn protocol_handler() -> Option<String> {
    let scheme = wide("t3code");
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: the buffer is writable for `length` characters.
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            ASSOCSTR_EXECUTABLE,
            scheme.as_ptr(),
            null(),
            buffer.as_mut_ptr(),
            &mut length,
        )
    };
    // The length includes the terminator.
    (result == 0 && length > 1).then(|| String::from_utf16_lossy(&buffer[..length as usize - 1]))
}

/// Hands a URL to its handler without a command shell.
pub fn shell_open(url: &str) -> bool {
    let (verb, url) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated, and the optional arguments may be null.
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            verb.as_ptr(),
            url.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    result as usize > 32
}

fn window_exe(window: HWND) -> Option<String> {
    // SAFETY: a stale window makes these calls fail, and the process handle is closed.
    unsafe {
        let mut pid = 0;
        if window.is_null() || GetWindowThreadProcessId(window, &mut pid) == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(process);
        (ok != 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
    }
}

pub fn is_foreground(exe: &str) -> bool {
    // SAFETY: takes no arguments and may return null, which `window_exe` handles.
    let window = unsafe { GetForegroundWindow() };
    window_exe(window).is_some_and(|path| path.eq_ignore_ascii_case(exe))
}

pub struct Window(HWND);

struct Search<'a> {
    exe: &'a str,
    best: Option<(i64, HWND)>,
}

unsafe extern "system" fn consider(window: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `Search` that `main_window` keeps alive during enumeration.
    unsafe {
        let search = &mut *(data as *mut Search);
        let candidate = IsWindowVisible(window) != 0
            && GetWindow(window, GW_OWNER).is_null()
            && GetWindowLongW(window, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW == 0;
        if !candidate {
            return 1;
        }
        // The restored size, so a minimized main window still outranks small helper windows.
        let mut placement = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        if GetWindowPlacement(window, &mut placement) == 0 {
            return 1;
        }
        let RECT {
            left,
            top,
            right,
            bottom,
        } = placement.rcNormalPosition;
        let area = i64::from(right - left) * i64::from(bottom - top);
        if search.best.is_none_or(|(best, _)| area > best)
            && window_exe(window).is_some_and(|path| path.eq_ignore_ascii_case(search.exe))
        {
            search.best = Some((area, window));
        }
        1
    }
}

/// The largest top-level window of `exe`.
pub fn main_window(exe: &str) -> Option<Window> {
    let mut search = Search { exe, best: None };
    // SAFETY: the callback only runs during this call, while `search` is alive.
    unsafe {
        EnumWindows(Some(consider), &raw mut search as LPARAM);
    }
    search.best.map(|(_, window)| Window(window))
}

impl Window {
    pub fn is_minimized(&self) -> bool {
        // SAFETY: a stale handle returns zero.
        unsafe { IsIconic(self.0) != 0 }
    }

    /// Brings the window forward by briefly sharing the foreground window's input queue, which
    /// is what lifts the foreground lock. No input is synthesized.
    pub fn raise(&self) -> bool {
        // SAFETY: stale handles make the calls fail, and a successful attach is always undone.
        unsafe {
            if IsIconic(self.0) != 0 {
                ShowWindowAsync(self.0, SW_RESTORE);
            }
            // AttachThreadInput needs this thread to have a message queue.
            let mut message = MSG::default();
            PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);

            let current = GetCurrentThreadId();
            let front = GetForegroundWindow();
            // Attaching to a hung window would hang this thread with it.
            let front_thread = if front.is_null() || IsHungAppWindow(front) != 0 {
                0
            } else {
                GetWindowThreadProcessId(front, null_mut())
            };
            let attached = front_thread != 0
                && front_thread != current
                && AttachThreadInput(current, front_thread, 1) != 0;
            let accepted = SetForegroundWindow(self.0) != 0;
            BringWindowToTop(self.0);
            if attached {
                AttachThreadInput(current, front_thread, 0);
            }
            accepted
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{protect, unprotect};

    #[test]
    fn dpapi_round_trip() {
        let secret = b"invented secret";
        let sealed = protect(secret).unwrap();
        assert!(!sealed.windows(secret.len()).any(|part| part == secret));
        assert_eq!(unprotect(&sealed).as_deref(), Some(&secret[..]));
        assert!(unprotect(b"not sealed").is_none());
    }
}
