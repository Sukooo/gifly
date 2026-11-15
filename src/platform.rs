//! The small amount of Win32 we need: show/hide at the cursor, restore focus, paste, single instance.
//!
//! Most `unsafe` here is a plain Win32 call on a window handle. Those can't cause undefined
//! behavior even when the window is gone: Windows checks handles, and the call just fails.

use std::{
    ffi::c_void,
    sync::atomic::{AtomicIsize, Ordering::Relaxed},
    thread::sleep,
    time::Duration,
};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND,
            POINT, RECT, WAIT_OBJECT_0,
        },
        Graphics::{
            Dwm::{
                DWMWA_BORDER_COLOR, DWMWA_CLOAK, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
            },
            Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
        },
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardSequenceNumber,
                IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
            Registry::{
                HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegDeleteKeyValueW,
                RegGetValueW, RegSetKeyValueW,
            },
            Threading::{
                CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, GetCurrentProcess, INFINITE, OpenEventW, SetEvent,
                SetProcessWorkingSetSize, WaitForSingleObject,
            },
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
                SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RETURN, VK_RWIN, VK_SHIFT, VK_V,
            },
            WindowsAndMessaging::{
                ASFW_ANY, AllowSetForegroundWindow, FindWindowW, GetCursorPos, GetForegroundWindow, GetWindowRect,
                HWND_TOPMOST, IsWindowVisible, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
                SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::{
    log::{self, Area},
    util::wait_until,
};

pub const TITLE: &str = "gifly";

/// A window handle that can be sent between threads (`HWND` is a raw pointer, so it can't).
/// It's only an id: Windows checks it on every call, so a stale one is harmless.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Hwnd(isize);

impl Hwnd {
    pub const NONE: Self = Self(0);

    pub fn of(w: &impl HasWindowHandle) -> Self {
        match w.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => Self(h.hwnd.get()),
            _ => Self::NONE,
        }
    }

    fn raw(self) -> HWND {
        HWND(self.0 as *mut c_void)
    }
}

impl From<HWND> for Hwnd {
    fn from(h: HWND) -> Self {
        Self(h.0 as isize)
    }
}

#[derive(Default)]
pub struct AtomicHwnd(AtomicIsize);

impl AtomicHwnd {
    pub fn get(&self) -> Hwnd {
        Hwnd(self.0.load(Relaxed))
    }

    pub fn set(&self, h: Hwnd) {
        self.0.store(h.0, Relaxed);
    }
}

pub fn foreground() -> Hwnd {
    // SAFETY: no arguments.
    unsafe { GetForegroundWindow() }.into()
}

pub fn is_visible(h: Hwnd) -> bool {
    // SAFETY: a handle only (see the module docs).
    unsafe { IsWindowVisible(h.raw()) }.as_bool()
}

pub fn hide(h: Hwnd) {
    // SAFETY: a handle only.
    let _ = unsafe { ShowWindow(h.raw(), SW_HIDE) };
}

pub fn round_corners(h: Hwnd) {
    // SAFETY: each attribute pointer points at a local of the size passed with it.
    unsafe {
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            h.raw(),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const pref).cast(),
            size_of_val(&pref) as u32,
        );
        // Subtle 1px outline instead of the accent-colored default (COLORREF is 0x00BBGGRR).
        let border: u32 = 0x0036_3532;
        let _ =
            DwmSetWindowAttribute(h.raw(), DWMWA_BORDER_COLOR, (&raw const border).cast(), size_of_val(&border) as u32);
    }
}

/// Keeps the window off the screen while it still counts as shown and draws as usual.
pub fn cloak(h: Hwnd, on: bool) {
    let v = i32::from(on);
    // SAFETY: the pointer points at a local of the size passed with it.
    let _ = unsafe { DwmSetWindowAttribute(h.raw(), DWMWA_CLOAK, (&raw const v).cast(), size_of_val(&v) as u32) };
}

/// Shows the window just above the cursor (or below, if there's no room), kept on-screen.
pub fn show_at_cursor(h: Hwnd) {
    let mut cursor = POINT::default();
    // SAFETY: an out-pointer to a local.
    let _ = unsafe { GetCursorPos(&mut cursor) };
    let r = window_rect(h).unwrap_or_else(|| {
        log::warn(Area::App, "couldn't read the picker's size to place it");
        RECT::default()
    });
    let (width, height) = (r.right - r.left, r.bottom - r.top);
    let work = work_area(cursor);
    let above = cursor.y - height - 12;
    let y = if above < work.top { cursor.y + 12 } else { above };
    // Left edge just under the cursor, so the picker opens toward the right (reading direction).
    let x = (cursor.x - 16).clamp(work.left, (work.right - width).max(work.left));
    let y = y.clamp(work.top, (work.bottom - height).max(work.top));
    // SAFETY: a handle only.
    unsafe {
        let _ = SetWindowPos(h.raw(), Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_SHOWWINDOW);
        let _ = ShowWindow(h.raw(), SW_SHOW);
        let _ = SetForegroundWindow(h.raw());
    }
}

fn window_rect(h: Hwnd) -> Option<RECT> {
    let mut r = RECT::default();
    // SAFETY: a handle, and an out-pointer to a local.
    unsafe { GetWindowRect(h.raw(), &mut r) }.ok().map(|()| r)
}

/// The work area (the screen minus the taskbar) of the monitor nearest `at`.
fn work_area(at: POINT) -> RECT {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: an out-pointer to a local, whose `cbSize` tells Windows how big it is.
    let _ = unsafe { GetMonitorInfoW(MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST), &mut info) };
    info.rcWork
}

/// Shows the window off every screen without activating it, for the soak test (see `app::soak`).
pub fn show_offscreen(h: Hwnd) {
    let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW;
    // SAFETY: a handle only.
    let _ = unsafe { SetWindowPos(h.raw(), None, -20000, 0, 0, 0, flags) };
}

/// Grows or shrinks the window by `factor`, keeping it inside the monitor's work area.
pub fn scale_window(h: Hwnd, factor: f32) {
    let Some(r) = window_rect(h) else { return };
    let work = work_area(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 });
    let scale = |v: i32, max: i32| ((v as f32 * factor).round() as i32).min(max);
    let width = scale(r.right - r.left, work.right - work.left);
    let height = scale(r.bottom - r.top, work.bottom - work.top);
    // Keep the bottom edge where it was: the picker usually sits just above the cursor.
    let x = r.left.clamp(work.left, (work.right - width).max(work.left));
    let y = (r.bottom - height).clamp(work.top, (work.bottom - height).max(work.top));
    // SAFETY: a handle only.
    let _ = unsafe { SetWindowPos(h.raw(), None, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE) };
}

/// The Windows build, like "26200.7171 (25H2)", for logs and diagnostics.
pub fn windows_version() -> String {
    const KEY: PCWSTR = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let text = |name: PCWSTR| {
        let mut buf = [0u16; 64];
        let mut len = size_of_val(&buf) as u32;
        let data = Some(buf.as_mut_ptr().cast());
        // SAFETY: `len` is the size of `buf` in bytes, and Windows writes no more than that.
        let res = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, KEY, name, RRF_RT_REG_SZ, None, data, Some(&mut len)) };
        // `len` counts bytes, with the closing nul.
        res.is_ok().then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
    };
    let mut ubr = 0u32;
    let mut len = size_of_val(&ubr) as u32;
    let data = Some((&raw mut ubr).cast());
    // SAFETY: `len` is the size of `ubr` in bytes.
    let res = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, KEY, w!("UBR"), RRF_RT_REG_DWORD, None, data, Some(&mut len)) };
    let mut v = text(w!("CurrentBuild")).unwrap_or_else(|| "?".into());
    if res.is_ok() {
        v += &format!(".{ubr}");
    }
    if let Some(name) = text(w!("DisplayVersion")) {
        v += &format!(" ({name})");
    }
    v
}

/// Opens Explorer with `path` selected.
pub fn reveal(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
}

/// Once the GPU has released what hiding freed, hands idle pages back to Windows so the tray
/// app sits small. They fault back in quickly when the picker opens again.
pub fn trim_memory_soon() {
    crate::spawn("trim-memory", || {
        sleep(Duration::from_millis(800));
        // SAFETY: our own process; `usize::MAX` twice means "trim", not a size.
        let _ = unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
    });
}

/// Adds gifly to (or removes it from) the apps Windows starts at sign-in. False if that failed.
pub fn set_autostart(on: bool) -> bool {
    let run = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    if !on {
        // SAFETY: static nul-terminated strings only.
        let e = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, run, w!("gifly")) };
        return e.is_ok() || e == ERROR_FILE_NOT_FOUND;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    let cmd: Vec<u16> = format!("\"{}\" --hidden", exe.display()).encode_utf16().chain([0]).collect();
    let bytes = (cmd.len() * 2) as u32;
    // SAFETY: `bytes` is the size of `cmd` (nul included), which outlives the call.
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, run, w!("gifly"), REG_SZ.0, Some(cmd.as_ptr().cast()), bytes) }.is_ok()
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                ..Default::default()
            },
        },
    }
}

/// Focuses `target` and sends Ctrl+V (and Enter). Keys only go to `target`: if it can't be focused,
/// nothing is sent. Returns whether the paste was sent. Blocks briefly; call off the UI thread.
pub fn paste_into(target: Hwnd, enter: bool) -> bool {
    let focused = || foreground() == target;
    // SAFETY: Windows reads `keys.len()` inputs of the size passed.
    let send = |keys: &[INPUT]| unsafe { SendInput(keys, size_of::<INPUT>() as i32) } == keys.len() as u32;
    if target == Hwnd::NONE {
        return false;
    }
    if !wait_for_modifiers_released() {
        log::warn(Area::Paste, "modifier keys are still held");
        return false;
    }
    // SAFETY: a handle only.
    let _ = unsafe { SetForegroundWindow(target.raw()) };
    // Let the focus switch settle before checking it.
    sleep(Duration::from_millis(60));
    if !wait_until(11, Duration::from_millis(30), focused) {
        log::warn(Area::Paste, "couldn't focus the target window");
        return false;
    }
    if !send(&[key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)]) {
        log::warn(Area::Paste, "Windows refused the keystrokes");
        return false;
    }
    if enter {
        // Give the target time to take the paste, or Enter sends an empty message.
        sleep(Duration::from_millis(60));
        if focused() {
            send(&[key(VK_RETURN, false), key(VK_RETURN, true)]);
        }
    }
    true
}

/// Whether Ctrl+V is down right now. egui only hears it as a text paste, which files aren't.
pub fn ctrl_v_down() -> bool {
    [VK_CONTROL, VK_V].into_iter().all(key_down)
}

fn key_down(k: VIRTUAL_KEY) -> bool {
    // SAFETY: a key code only.
    unsafe { GetAsyncKeyState(i32::from(k.0)) < 0 }
}

/// Waits (up to half a second) until no modifier is held. A Shift still down from Shift+click
/// would otherwise turn our Ctrl+V and Enter into Ctrl+Shift+V and Shift+Enter (a newline).
fn wait_for_modifiers_released() -> bool {
    let held = || [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(key_down);
    wait_until(26, Duration::from_millis(20), || !held())
}

/// True if another gifly is running. The mutex is never closed, so it's held until exit.
pub fn already_running() -> bool {
    // SAFETY: a static name; `GetLastError` is read right after the call that set it.
    unsafe {
        let _ = CreateMutexW(None, true, w!("gifly-single-instance"));
        GetLastError() == ERROR_ALREADY_EXISTS
    }
}

/// Set by a second launch to ask the running gifly to open its picker.
const RELAUNCH_EVENT: PCWSTR = w!("gifly-open");

/// Calls `open` (on its own thread) each time gifly is started while this copy is running.
pub fn on_relaunch(open: impl Fn() + Send + 'static) {
    // Auto-reset, so each launch opens the picker once.
    // SAFETY: a static name.
    let Ok(event) = (unsafe { CreateEventW(None, false, false, RELAUNCH_EVENT) }) else { return };
    let raw = event.0 as isize; // HANDLE isn't Send
    crate::spawn("relaunch", move || {
        // SAFETY: the event is never closed, so the handle stays valid until the process ends.
        while unsafe { WaitForSingleObject(HANDLE(raw as *mut c_void), INFINITE) } == WAIT_OBJECT_0 {
            open();
        }
    });
}

/// Asks the running gifly to open its picker (see `on_relaunch`).
pub fn open_existing() {
    // SAFETY: static names, and a handle this function opens and closes itself.
    unsafe {
        // Windows lets a process that was just started bring a window to the front. Pass that on.
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, RELAUNCH_EVENT) {
            let _ = SetEvent(event);
            let _ = CloseHandle(event);
        } else if let Ok(h) = FindWindowW(w!("Window Class"), w!("gifly")) {
            // A gifly from before the event existed: just show its window. Matching winit's
            // window class means an Explorer window on a folder named "gifly" doesn't count.
            show_at_cursor(h.into());
        }
    }
}

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIBV5: u32 = 17;
/// A clipboard bigger than this isn't kept for restoring.
const SNAPSHOT_MAX: usize = 64 << 20;

/// Bumped by Windows on every clipboard change.
pub fn clipboard_seq() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetClipboardSequenceNumber() }
}

/// Closes the clipboard when dropped.
struct OpenClipboardGuard;

impl OpenClipboardGuard {
    /// Opens the clipboard, retrying briefly while another app has it open.
    fn new(owner: Hwnd) -> Option<Self> {
        // SAFETY: a handle only.
        wait_until(10, Duration::from_millis(10), || unsafe { OpenClipboard(Some(owner.raw())) }.is_ok())
            .then_some(Self)
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: the guard exists only while we have the clipboard open.
        let _ = unsafe { CloseClipboard() };
    }
}

fn registered<const N: usize>(names: [PCWSTR; N]) -> impl Iterator<Item = u32> {
    // SAFETY: `w!` names are static and nul-terminated.
    names.into_iter().map(|n| unsafe { RegisterClipboardFormatW(n) }).filter(|&f| f != 0)
}

/// What was on the clipboard, to put back after pasting a link: text, images, rich text, HTML
/// and copied files. Other formats (Office has dozens of its own) are left out, since reading
/// one makes the app that copied it render it, which can take a while.
pub struct ClipboardSnapshot {
    formats: Vec<(u32, Vec<u8>)>,
}

impl ClipboardSnapshot {
    pub fn take(owner: Hwnd) -> Option<Self> {
        let _open = OpenClipboardGuard::new(owner)?;
        let extra: Vec<u32> = registered([w!("PNG"), w!("HTML Format"), w!("Rich Text Format")]).collect();
        // One bitmap format is enough; Windows makes the others from it.
        // SAFETY: a format id only.
        let image = [CF_DIBV5, CF_DIB].into_iter().find(|&f| unsafe { IsClipboardFormatAvailable(f) }.is_ok());
        let wanted = |f: u32| f == CF_UNICODETEXT || f == CF_HDROP || Some(f) == image || extra.contains(&f);
        let (mut formats, mut total, mut f) = (Vec::new(), 0, 0);
        // In the order the app that copied them listed them, which some apps rely on.
        loop {
            // SAFETY: the clipboard is open (`_open`), which this needs.
            f = unsafe { EnumClipboardFormats(f) };
            if f == 0 {
                break;
            }
            if !wanted(f) {
                continue;
            }
            // SAFETY: the clipboard is open. It owns `mem` until it closes, after we're done.
            let Ok(h) = (unsafe { GetClipboardData(f) }) else { continue };
            let mem = HGLOBAL(h.0);
            // SAFETY: as above.
            let size = unsafe { GlobalSize(mem) };
            if size == 0 {
                continue;
            }
            // SAFETY: as above.
            let p = unsafe { GlobalLock(mem) };
            if p.is_null() {
                continue;
            }
            total += size;
            // SAFETY: locked, `p` points at the `size` bytes `mem` holds and they can't move.
            let bytes =
                (total <= SNAPSHOT_MAX).then(|| unsafe { std::slice::from_raw_parts(p.cast::<u8>(), size) }.to_vec());
            // SAFETY: locked just above.
            let _ = unsafe { GlobalUnlock(mem) };
            formats.push((f, bytes?));
        }
        (!formats.is_empty()).then_some(Self { formats })
    }

    pub fn text(&self) -> Option<String> {
        let (_, b) = self.formats.iter().find(|(f, _)| *f == CF_UNICODETEXT)?;
        let wide: Vec<u16> =
            b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
        Some(String::from_utf16_lossy(&wide))
    }

    /// Puts everything back if the clipboard hasn't changed, marked to stay out of clipboard
    /// history and clipboard managers, since it may be a password.
    pub fn restore(&self, owner: Hwnd, expected_seq: u32) -> bool {
        let Some(_open) = OpenClipboardGuard::new(owner) else { return false };
        // Check while holding the clipboard open: another app may have copied something while
        // we waited for it, and that copy must never be overwritten by our old snapshot.
        if clipboard_seq() != expected_seq {
            return false;
        }
        // SAFETY: the clipboard is open (`_open`).
        if unsafe { EmptyClipboard() }.is_err() {
            return false;
        }
        let off = 0u32.to_le_bytes();
        let private = registered([
            w!("ExcludeClipboardContentFromMonitorProcessing"),
            w!("CanIncludeInClipboardHistory"),
            w!("CanUploadToCloudClipboard"),
        ])
        .map(|f| (f, &off[..]));
        // Every format is tried even after one fails, and the privacy markers first, so a
        // password never reaches clipboard history because some other format didn't fit.
        let all = private.into_iter().chain(self.formats.iter().map(|(f, b)| (*f, b.as_slice())));
        all.fold(true, |ok, (f, b)| set_clipboard(f, b) && ok)
    }
}

/// Call with the clipboard open.
fn set_clipboard(format: u32, bytes: &[u8]) -> bool {
    // SAFETY: `mem` holds at least `bytes.len()` bytes and is locked while they're copied in.
    // It's freed only if the clipboard didn't take it.
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else { return false };
        let p = GlobalLock(mem);
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(mem);
        // Once this succeeds the clipboard owns the memory.
        if SetClipboardData(format, Some(HANDLE(mem.0))).is_err() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_version_reads_the_build() {
        let v = super::windows_version();
        assert!(v.starts_with(|c: char| c.is_ascii_digit()) && v.contains('.'), "{v}");
    }
}
