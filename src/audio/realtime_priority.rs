//! Best-effort Windows multimedia scheduling for the non-realtime decoder thread.
//!
//! The WASAPI/CPAL callback belongs to its own backend thread. Only the PCM
//! producer gets an MMCSS task lease here; never boost the UI, network,
//! background preload or whole process, and never use REALTIME_PRIORITY_CLASS.
#[cfg(windows)]
#[allow(unsafe_code)] // SAFETY: only the documented avrt MMCSS FFI surface below.
mod platform {
    use core::ffi::c_void;

    #[link(name = "avrt")]
    unsafe extern "system" {
        fn AvSetMmThreadCharacteristicsW(
            task_name: *const u16,
            task_index: *mut u32,
        ) -> *mut c_void;
        fn AvRevertMmThreadCharacteristics(handle: *mut c_void) -> i32;
    }

    pub(crate) struct AudioDecodePriority {
        task_handle: *mut c_void,
    }

    impl AudioDecodePriority {
        pub(crate) fn enter() -> Self {
            const AUDIO_TASK: [u16; 6] = [
                b'A' as u16,
                b'u' as u16,
                b'd' as u16,
                b'i' as u16,
                b'o' as u16,
                0,
            ];
            let mut task_index = 0;
            // SAFETY: a valid, NUL-terminated UTF-16 string and a writable
            // DWORD are passed. The returned task handle is released on this
            // same worker thread, even if playback exits through an error.
            let task_handle =
                unsafe { AvSetMmThreadCharacteristicsW(AUDIO_TASK.as_ptr(), &mut task_index) };
            if task_handle.is_null() {
                tracing::debug!("Audio MMCSS task unavailable; using normal thread priority");
            }
            Self { task_handle }
        }
    }

    impl Drop for AudioDecodePriority {
        fn drop(&mut self) {
            if !self.task_handle.is_null() {
                // SAFETY: this handle was returned by
                // AvSetMmThreadCharacteristicsW on the same OS thread.
                let _ = unsafe { AvRevertMmThreadCharacteristics(self.task_handle) };
            }
        }
    }
}

#[cfg(windows)]
pub(crate) use platform::AudioDecodePriority;

#[cfg(not(windows))]
pub(super) struct AudioDecodePriority;

#[cfg(not(windows))]
impl AudioDecodePriority {
    #[inline]
    pub(super) fn enter() -> Self {
        Self
    }
}
