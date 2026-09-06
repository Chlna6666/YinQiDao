#![cfg_attr(windows, allow(unsafe_code))]

#[cfg(windows)]
use std::ffi::c_void;

#[cfg(windows)]
fn native_hwnd(window: &gpui::Window) -> Option<*mut c_void> {
    use raw_window_handle::RawWindowHandle;

    // `gpui::Window` also has an inherent `window_handle()` returning GPUI's logical handle.
    // Call raw-window-handle explicitly so this always resolves to the native Win32 HWND.
    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.hwnd.get() as *mut c_void)
}

#[cfg(windows)]
fn remove_overlay_non_client_chrome(hwnd: *mut c_void) {
    const DWMWA_NCRENDERING_POLICY: u32 = 2;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWA_BORDER_COLOR: u32 = 34;

    const DWMNCRP_DISABLED: u32 = 1;
    const DWMWCP_DONOTROUND: u32 = 1;
    const DWMWA_COLOR_NONE: u32 = 0xffff_fffe;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut c_void,
            attribute: u32,
            value: *const c_void,
            value_size: u32,
        ) -> i32;
    }

    fn apply(hwnd: *mut c_void, attribute: u32, value: &u32) {
        // SAFETY: `hwnd` is obtained from a live GPUI Win32 window. DWM copies the fixed-size u32
        // attribute value during this call and does not retain the pointer.
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                attribute,
                (value as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }

    // A desktop lyric surface is fully client-drawn and alpha-composited. Native non-client
    // rendering only adds a border/rounded frame/drop shadow around otherwise transparent pixels.
    apply(hwnd, DWMWA_NCRENDERING_POLICY, &DWMNCRP_DISABLED);
    apply(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &DWMWCP_DONOTROUND,
    );
    apply(hwnd, DWMWA_BORDER_COLOR, &DWMWA_COLOR_NONE);
}

#[cfg(windows)]
fn apply_desktop_lyrics_extended_style(hwnd: *mut c_void) {
    const GWL_EXSTYLE: i32 = -20;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_APPWINDOW: u32 = 0x0004_0000;
    const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowLongW(hwnd: *mut c_void, index: i32) -> i32;
        fn SetWindowLongW(hwnd: *mut c_void, index: i32, value: i32) -> i32;
    }

    // TOOLWINDOW keeps this independent overlay out of the taskbar and Alt-Tab. NOACTIVATE keeps
    // mouse interaction from stealing focus from the application underneath the desktop lyrics.
    // SAFETY: the HWND belongs to a live GPUI window and these style calls do not retain pointers.
    unsafe {
        let current = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let desired = (current | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) & !WS_EX_APPWINDOW;
        if desired != current {
            let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, desired as i32);
        }
    }
}

#[cfg(windows)]
fn apply_topmost(hwnd: *mut c_void, enabled: bool) -> bool {
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_FRAMECHANGED: u32 = 0x0020;
    const SWP_NOOWNERZORDER: u32 = 0x0200;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetWindowPos(
            hwnd: *mut c_void,
            hwnd_insert_after: *mut c_void,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
    }

    let insert_after = if enabled {
        (-1_isize) as *mut c_void // HWND_TOPMOST
    } else {
        (-2_isize) as *mut c_void // HWND_NOTOPMOST
    };
    let flags =
        SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_NOOWNERZORDER;

    // SAFETY: `hwnd` comes from the live GPUI window. SetWindowPos does not retain either handle;
    // NOMOVE/NOSIZE/NOACTIVATE keep this operation limited to z-order and frame recomputation.
    unsafe { SetWindowPos(hwnd, insert_after, 0, 0, 0, 0, flags) != 0 }
}

#[cfg(windows)]
pub(crate) fn configure_desktop_lyrics_window(
    window: &gpui::Window,
    always_on_top: bool,
) -> bool {
    let Some(hwnd) = native_hwnd(window) else {
        return false;
    };

    apply_desktop_lyrics_extended_style(hwnd);
    remove_overlay_non_client_chrome(hwnd);
    apply_topmost(hwnd, always_on_top)
}

#[cfg(not(windows))]
pub(crate) fn configure_desktop_lyrics_window(
    window: &gpui::Window,
    always_on_top: bool,
) -> bool {
    set_always_on_top(window, always_on_top)
}

#[cfg(windows)]
pub(crate) fn set_always_on_top(window: &gpui::Window, enabled: bool) -> bool {
    let Some(hwnd) = native_hwnd(window) else {
        return false;
    };
    remove_overlay_non_client_chrome(hwnd);
    apply_topmost(hwnd, enabled)
}

#[cfg(not(windows))]
pub(crate) fn set_always_on_top(_window: &gpui::Window, _enabled: bool) -> bool {
    false
}
