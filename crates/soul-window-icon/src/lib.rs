//! Keep a live window's icon following the Windows **taskbar** theme, by raw HWND.
//!
//! Rune (and winit) only apply an app icon at window *creation*, and winit does not
//! reliably emit a theme-change event on Windows, so neither a one-shot set nor a
//! `view`-driven hook keeps the taskbar/Alt+Tab mark right when the user flips the
//! OS between light and dark. Instead a background thread blocks on
//! `RegNotifyChangeKeyValue` for the theme key and, the instant it changes, re-points
//! the window icon via `WM_SETICON`. No polling, no dependency on the UI redrawing.
//!
//! # Why a separate crate
//!
//! The player UI crate is `#![forbid(unsafe_code)]`, and the Win32 calls here are
//! `unsafe`. Confining them lets the UI crate stay unsafe-free and just call the safe
//! `watch_taskbar_icon` wrapper (the split `alloc-probe` uses for its allocator).

/// Whether the Windows **taskbar** is dark right now, i.e.
/// `Personalize\SystemUsesLightTheme == 0` (that value, not `AppsUseLightTheme`, is
/// the one that colours the taskbar and notification area). Missing/unreadable, or
/// off Windows → `true` (dark), the Windows default, so the caller picks the
/// white-bodied mark.
#[cfg(windows)]
pub fn taskbar_dark() -> bool {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, RegCloseKey, RegOpenKeyExW,
    };
    use windows::core::w;

    let mut hkey = HKEY::default();
    // SAFETY: standard key path; `hkey` receives the handle, closed below.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            None,
            KEY_QUERY_VALUE,
            &mut hkey,
        )
    };
    if opened != ERROR_SUCCESS {
        return true;
    }
    let dark = read_taskbar_dark(hkey);
    // SAFETY: `hkey` was opened above and is unused after this.
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    dark
}

/// Start following the Windows taskbar theme: sets `hwnd`'s icon immediately to the
/// variant that contrasts the current theme, then updates it on every later flip,
/// for the app's lifetime. `hwnd` is the raw Win32 handle as `isize`. The PNG bytes
/// are read straight by `CreateIconFromResourceEx` (the Vista PNG icon form).
///
/// `on_change(dark)` is invoked on each *later* flip (not the initial set) so the
/// caller can follow with surfaces this crate can't reach, notably the system tray,
/// whose icon Rune can't repaint in place, so it is recreated. No-op off Windows.
#[cfg(windows)]
pub fn watch_taskbar_icon(
    hwnd: isize,
    dark_png: &'static [u8],
    light_png: &'static [u8],
    on_change: impl Fn(bool) + Send + 'static,
) {
    if hwnd == 0 {
        return;
    }
    std::thread::Builder::new()
        .name("taskbar-theme-watch".into())
        .spawn(move || watch(hwnd, dark_png, light_png, on_change))
        .ok();
}

#[cfg(windows)]
fn watch(hwnd: isize, dark_png: &'static [u8], light_png: &'static [u8], on_change: impl Fn(bool)) {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_QUERY_VALUE, REG_NOTIFY_CHANGE_LAST_SET,
        RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW,
    };
    use windows::core::w;

    let mut hkey = HKEY::default();
    // SAFETY: standard key path; `hkey` receives the opened handle, closed below.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            None,
            KEY_NOTIFY | KEY_QUERY_VALUE,
            &mut hkey,
        )
    };
    if opened != ERROR_SUCCESS {
        return;
    }

    let mut last: Option<bool> = None;
    loop {
        let dark = read_taskbar_dark(hkey);
        if last != Some(dark) {
            set_window_icon(hwnd, if dark { dark_png } else { light_png });
            // The initial set matches what the caller already built; only a real
            // flip asks it to follow (recreate the tray, etc.).
            if last.is_some() {
                on_change(dark);
            }
            last = Some(dark);
        }
        // Block until the key changes (synchronous notify: no event, not async).
        // SAFETY: `hkey` is open with KEY_NOTIFY for the duration of the loop.
        let changed = unsafe {
            RegNotifyChangeKeyValue(hkey, false, REG_NOTIFY_CHANGE_LAST_SET, None, false)
        };
        if changed != ERROR_SUCCESS {
            break;
        }
    }
    // SAFETY: `hkey` was opened above and is not used after this.
    unsafe {
        let _ = RegCloseKey(hkey);
    }
}

/// Read `SystemUsesLightTheme` from an already-open Personalize key: dark = `0`.
#[cfg(windows)]
fn read_taskbar_dark(hkey: windows::Win32::System::Registry::HKEY) -> bool {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{REG_VALUE_TYPE, RegQueryValueExW};
    use windows::core::w;

    let mut data: u32 = 0;
    let mut size: u32 = 4;
    let mut kind = REG_VALUE_TYPE::default();
    // SAFETY: `data` is a 4-byte DWORD buffer and `size` is its length; the call
    // writes at most `size` bytes and updates `size` to what it wrote.
    let read = unsafe {
        RegQueryValueExW(
            hkey,
            w!("SystemUsesLightTheme"),
            None,
            Some(&mut kind),
            Some(&mut data as *mut u32 as *mut u8),
            Some(&mut size),
        )
    };
    if read == ERROR_SUCCESS {
        data == 0
    } else {
        true
    }
}

/// Point the window's big (taskbar / Alt+Tab) and small (title-bar) icons at the
/// given PNG. The previous pair is freed on the next call, never before, so the
/// window is never left pointing at a destroyed icon.
#[cfg(windows)]
fn set_window_icon(hwnd: isize, png: &[u8]) {
    use std::sync::Mutex;

    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconFromResourceEx, DestroyIcon, HICON, ICON_BIG, ICON_SMALL, LR_DEFAULTCOLOR,
        SendMessageW, WM_SETICON,
    };

    static CURRENT: Mutex<Option<(isize, isize)>> = Mutex::new(None);

    let hwnd = HWND(hwnd as *mut _);
    let make = |size: i32| -> Option<HICON> {
        // SAFETY: `png` is a valid borrowed slice for the call, which copies the
        // pixels into a newly-owned HICON (or returns Err).
        unsafe {
            CreateIconFromResourceEx(png, true, 0x0003_0000, size, size, LR_DEFAULTCOLOR).ok()
        }
    };
    let (Some(big), Some(small)) = (make(256), make(32)) else {
        return;
    };
    // SAFETY: `hwnd` is the live top-level window passed by the caller. WM_SETICON
    // records the handle for the window to draw with; it does not take ownership, so
    // the previous pair is freed by us below. Cross-thread SendMessage marshals to
    // the window's own thread.
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(big.0 as isize)),
        );
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(small.0 as isize)),
        );
    }
    if let Some((prev_big, prev_small)) = CURRENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .replace((big.0 as isize, small.0 as isize))
    {
        // SAFETY: our own icons, no longer shown (the window adopted the new pair).
        unsafe {
            let _ = DestroyIcon(HICON(prev_big as *mut _));
            let _ = DestroyIcon(HICON(prev_small as *mut _));
        }
    }
}

/// Off Windows: assume dark (the common default), so the caller picks the
/// white-bodied mark for its one static icon.
#[cfg(not(windows))]
pub fn taskbar_dark() -> bool {
    true
}

/// Off Windows there is no live taskbar-icon change here (macOS swaps the Dock image
/// through Rune's own `set_app_icon`; Linux takes its icon from the `.desktop` file).
#[cfg(not(windows))]
pub fn watch_taskbar_icon(
    _hwnd: isize,
    _dark_png: &'static [u8],
    _light_png: &'static [u8],
    _on_change: impl Fn(bool) + Send + 'static,
) {
}
