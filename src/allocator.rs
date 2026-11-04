//! 分配器回收节奏调优（mimalloc）。
//!
//! mimalloc 会在页面空闲一段时间后自动 purge（decommit）归还给 OS，默认延迟
//! 1000ms（见 libmimalloc-sys 内置 mimalloc 源码 `options.c` 的 `purge_delay`）。
//! 播放器进程的分配高度突发：曲库扫描、封面批处理、解码缓冲都会瞬间产生数十
//! MB 峰值随后长期空闲。把 purge 延迟收紧到 200ms，让这些突发空闲页更快归还
//! OS，降低常驻/私有内存峰值；代价是突发-空闲交替时略多的系统调用。
//!
//! 这里只自声明最小 FFI 面（`mi_option_get`/`mi_option_set`）：libmimalloc
//! 静态库经由 mimalloc crate 链入，无需引入 `extended` 绑定或新依赖。

/// `mi_option_t::mi_option_purge_delay` 在 mimalloc v2/v3 枚举中的序号
/// （见 libmimalloc-sys 内置 `mimalloc/include/mimalloc.h`）。
const MI_OPTION_PURGE_DELAY: core::ffi::c_int = 15;
/// 空闲页 purge（归还 OS）延迟，毫秒。mimalloc 默认 1000。
const MIMALLOC_PURGE_DELAY_MS: core::ffi::c_long = 200;
/// purge_delay 的出厂默认值，用作枚举序号未漂移的运行时校验锚点。
const MIMALLOC_PURGE_DELAY_DEFAULT_MS: core::ffi::c_long = 1000;

#[allow(unsafe_code)]
unsafe extern "C" {
    fn mi_option_get(option: core::ffi::c_int) -> core::ffi::c_long;
    fn mi_option_set(option: core::ffi::c_int, value: core::ffi::c_long);
}

/// 必须在启动早期、大量分配发生前调用，才能覆盖第一波突发分配。
pub(crate) fn tune_mimalloc_reclamation() {
    // SAFETY: `mi_option_get`/`mi_option_set` 仅读写进程级分配器选项，任意线程、
    // 任意时机调用均安全；不分配内存，也不引入跨分配器释放问题。先读默认值
    // 校验枚举序号未漂移，漂移时宁可不调优也不写错选项。
    #[allow(unsafe_code)]
    unsafe {
        if mi_option_get(MI_OPTION_PURGE_DELAY) == MIMALLOC_PURGE_DELAY_DEFAULT_MS {
            mi_option_set(MI_OPTION_PURGE_DELAY, MIMALLOC_PURGE_DELAY_MS);
        }
    }
}
