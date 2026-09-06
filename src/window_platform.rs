#![cfg_attr(windows, allow(unsafe_code))]

#[cfg(windows)]
pub(crate) fn set_always_on_top(window: &gpui::Window, enabled: bool) -> bool {
    use std::ffi::c_void;

    use raw_window_handle::RawWindowHandle;

    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOACTIVATE: u32 = 0x0010;
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

    // `gpui::Window` also exposes an inherent `window_handle()` that returns GPUI's
    // `AnyWindowHandle`. Use the raw-window-handle trait explicitly so we get the native
    // Win32 handle rather than accidentally resolving the inherent method.
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return false;
    };

    let hwnd = handle.hwnd.get() as *mut c_void;
    let insert_after = if enabled {
        (-1_isize) as *mut c_void // HWND_TOPMOST
    } else {
        (-2_isize) as *mut c_void // HWND_NOTOPMOST
    };
    let flags = SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;

    // SAFETY: `hwnd` comes from the live GPUI window's Win32 raw handle. `SetWindowPos` does not
    // retain either pointer, and NOMOVE/NOSIZE/NOACTIVATE ensure this call only changes Z-order.
    unsafe { SetWindowPos(hwnd, insert_after, 0, 0, 0, 0, flags) != 0 }
}

#[cfg(not(windows))]
pub(crate) fn set_always_on_top(_window: &gpui::Window, _enabled: bool) -> bool {
    false
}
