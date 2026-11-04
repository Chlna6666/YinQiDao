use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::Cursor,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::Duration,
};

use gpui::{
    AnyElement, App, Context, EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, Styled, div,
    img, prelude::*, px,
};
use lucide_gpui::icon;

use super::theme::{self, ACCENT_RED, BORDER_HAIRLINE, TEXT_SECONDARY, themed_icon};

/// 远端图片内存缓存条目上限（512px PNG 约 50–300KB/张）。FIFO 驱逐：渲染
/// working set（首屏与当前歌单）远小于该上限，不会抖动；被驱逐的图片仍可
/// 从磁盘缓存秒回（`load_disk_cached`）。
const MEM_CACHE_MAX_ENTRIES: usize = 256;

#[derive(Default)]
struct MemCache {
    map: HashMap<String, Arc<[u8]>>,
    /// FIFO 驱逐序，与 map 的键集同步维护。
    order: VecDeque<String>,
}

impl MemCache {
    fn get(&self, url: &str) -> Option<Arc<[u8]>> {
        self.map.get(url).cloned()
    }

    fn insert(&mut self, url: String, bytes: Arc<[u8]>) {
        if self.map.insert(url.clone(), bytes).is_none() {
            self.order.push_back(url);
        }
        while self.order.len() > MEM_CACHE_MAX_ENTRIES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }
}

static MEM_CACHE: OnceLock<RwLock<MemCache>> = OnceLock::new();
static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static DISK_DIR: OnceLock<PathBuf> = OnceLock::new();
static REMOTE_IMAGE_CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();

const PREFETCH_VISIBLE_LIMIT: usize = 8;
const PREFETCH_BATCH_SIZE: usize = 4;

fn mem_cache() -> &'static RwLock<MemCache> {
    MEM_CACHE.get_or_init(|| RwLock::new(MemCache::default()))
}

fn in_flight() -> &'static Mutex<HashSet<String>> {
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn init_disk_cache(path: PathBuf) {
    let _ = fs::create_dir_all(&path);
    let _ = DISK_DIR.set(path);
}

fn url_disk_filename(url: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}.png")
}

/// Render-safe cache lookup. This function never touches the filesystem.
pub fn get_cached(url: &str) -> Option<Arc<[u8]>> {
    mem_cache().read().ok().and_then(|cache| cache.get(url))
}

/// Blocking disk lookup for worker threads only. A hit is promoted into the in-memory cache.
pub fn load_disk_cached(url: &str) -> Option<Arc<[u8]>> {
    if let Some(cached) = get_cached(url) {
        return Some(cached);
    }

    let disk_dir = DISK_DIR.get()?;
    let file_path = disk_dir.join(url_disk_filename(url));
    let bytes = fs::read(file_path).ok()?;
    if !bytes.starts_with(b"\x89PNG") {
        return None;
    }

    let arc: Arc<[u8]> = bytes.into();
    if let Ok(mut cache) = mem_cache().write() {
        cache.insert(url.to_string(), arc.clone());
    }
    Some(arc)
}

async fn load_or_fetch(url: String) -> Option<Arc<[u8]>> {
    let disk_url = url.clone();
    if let Ok(Some(bytes)) = tokio::task::spawn_blocking(move || load_disk_cached(&disk_url)).await
    {
        return Some(bytes);
    }

    let bytes = fetch_and_normalize(&url).await?;
    let arc: Arc<[u8]> = bytes.into();

    if let Some(disk_dir) = DISK_DIR.get() {
        let path = disk_dir.join(url_disk_filename(&url));
        let write_bytes = arc.clone();
        let _ = tokio::task::spawn_blocking(move || fs::write(path, write_bytes.as_ref())).await;
    }

    if let Ok(mut cache) = mem_cache().write() {
        cache.insert(url, arc.clone());
    }
    Some(arc)
}

pub fn load_remote_image<V: 'static>(url: &str, cx: &Context<V>) -> Option<Arc<[u8]>> {
    if let Some(cached) = get_cached(url) {
        return Some(cached);
    }

    let should_fetch = {
        let Ok(mut set) = in_flight().lock() else {
            return None;
        };
        set.insert(url.to_string())
    };

    if should_fetch {
        let fetch_url = url.to_string();
        let target_url = url.to_string();
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            let res = load_or_fetch(fetch_url.clone()).await;
            Ok((fetch_url, res))
        });

        cx.spawn(async move |this, cx| match task.await {
            Ok((url_done, Some(_))) => {
                if let Ok(mut set) = in_flight().lock() {
                    set.remove(&url_done);
                }
                let _ = this.update(cx, |_, cx| {
                    cx.notify();
                });
            }
            _ => {
                if let Ok(mut set) = in_flight().lock() {
                    set.remove(&target_url);
                }
            }
        })
        .detach();
    }

    None
}

/// Fire-and-forget fetch that wakes `sink` when the bytes land (or when the attempt fails and
/// releases its in-flight slot, letting a later render retry).
///
/// Takes `&App` so it works from every render context — page renders that already borrow the
/// app state, and `uniform_list` item closures that only receive `&mut App`.
pub fn request_remote_image<V: 'static>(url: &str, sink: gpui::WeakEntity<V>, cx: &App) {
    if get_cached(url).is_some() {
        return;
    }
    let should_fetch = {
        let Ok(mut set) = in_flight().lock() else {
            return;
        };
        set.insert(url.to_string())
    };
    if !should_fetch {
        return;
    }

    let fetch_url = url.to_string();
    let task = gpui_tokio::Tokio::spawn(cx, async move {
        let bytes = load_or_fetch(fetch_url.clone()).await;
        if let Ok(mut set) = in_flight().lock() {
            set.remove(&fetch_url);
        }
        bytes.is_some()
    });

    cx.spawn(async move |cx: &mut gpui::AsyncApp| {
        let _ = task.await;
        let _ = sink.update(cx, |_, cx| {
            cx.notify();
        });
    })
    .detach();
}

fn remote_image_client() -> Option<&'static reqwest::Client> {
    REMOTE_IMAGE_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .ok()
        })
        .as_ref()
}

fn normalize_remote_image(body: Vec<u8>) -> Option<Vec<u8>> {
    let img = image::load_from_memory(&body).ok()?;
    let img = if img.width() > 512 || img.height() > 512 {
        img.thumbnail(512, 512)
    } else {
        img
    };

    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

async fn fetch_and_normalize(url: &str) -> Option<Vec<u8>> {
    let client = remote_image_client()?;
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.bytes().await.ok()?.to_vec();

    // Image decode/resize/PNG encoding is CPU-heavy and must not occupy the async runtime workers
    // that also drive plugin RPC/network requests.
    tokio::task::spawn_blocking(move || normalize_remote_image(body))
        .await
        .ok()
        .flatten()
}

pub fn render_remote_avatar<V: 'static>(
    url: Option<&str>,
    size_px: f32,
    is_default: bool,
    cx: &Context<V>,
) -> AnyElement {
    let border_col = if is_default {
        ACCENT_RED
    } else {
        BORDER_HAIRLINE
    };

    let Some(url) = url.filter(|u| !u.trim().is_empty()) else {
        return div()
            .size(px(size_px))
            .rounded_full()
            .bg(theme::BG_CANVAS)
            .border_1()
            .border_color(border_col)
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .child(themed_icon(
                icon!(user),
                (size_px * 0.45).clamp(12.0, 36.0),
                TEXT_SECONDARY.into(),
            ))
            .into_any_element();
    };

    if let Some(png_bytes) = load_remote_image(url, cx) {
        img(EncodedImageBytes::new(ImageFormat::Png, png_bytes.clone()))
            .size(px(size_px))
            .rounded_full()
            .border_1()
            .border_color(border_col)
            .object_fit(ObjectFit::Cover)
            .flex_none()
            .into_any_element()
    } else {
        div()
            .size(px(size_px))
            .rounded_full()
            .bg(theme::BG_CANVAS)
            .border_1()
            .border_color(border_col)
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .child(themed_icon(
                icon!(user),
                (size_px * 0.45).clamp(12.0, 36.0),
                TEXT_SECONDARY.into(),
            ))
            .into_any_element()
    }
}

/// Spawns a background fetch without waking any view. Kept for fire-and-forget warmers;
/// render paths should prefer the notifying `load_remote_image` so covers appear as soon
/// as their bytes land.
pub fn fetch_detached(url: &str) {
    if get_cached(url).is_some() {
        return;
    }
    let should_fetch = {
        let Ok(mut set) = in_flight().lock() else {
            return;
        };
        set.insert(url.to_string())
    };
    if should_fetch {
        let fetch_url = url.to_string();
        match crate::runtime::io_handle() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = load_or_fetch(fetch_url.clone()).await;
                    if let Ok(mut set) = in_flight().lock() {
                        set.remove(&fetch_url);
                    }
                });
            }
            Err(_) => {
                // Never strand an URL in IN_FLIGHT when the runtime is unavailable; otherwise every
                // later render would believe a loader still owns it and the image would never retry.
                if let Ok(mut set) = in_flight().lock() {
                    set.remove(url);
                }
            }
        }
    }
}

pub fn prefetch_urls<V: 'static>(urls: Vec<String>, cx: &mut Context<V>) {
    let mut seen = HashSet::new();
    let urls_to_fetch: Vec<String> = urls
        .into_iter()
        .filter(|url| {
            !url.trim().is_empty() && get_cached(url).is_none() && seen.insert(url.clone())
        })
        .take(PREFETCH_VISIBLE_LIMIT)
        .collect();
    if urls_to_fetch.is_empty() {
        return;
    }

    // Warm only the first-screen working set. Two small batches can overlap without turning a
    // 100-track playlist into 100 background image jobs. Each batch triggers one repaint when its
    // bytes are resident, so visible covers appear progressively without per-image root churn.
    for batch in urls_to_fetch.chunks(PREFETCH_BATCH_SIZE) {
        let batch = batch.to_vec();
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            for url in batch {
                let should_fetch = {
                    let Ok(mut set) = in_flight().lock() else {
                        continue;
                    };
                    set.insert(url.clone())
                };
                if should_fetch {
                    let _ = load_or_fetch(url.clone()).await;
                    if let Ok(mut set) = in_flight().lock() {
                        set.remove(&url);
                    }
                }
            }
            Ok(())
        });

        cx.spawn(async move |this, cx| {
            let _ = task.await;
            let _ = this.update(cx, |_, cx| {
                cx.notify();
            });
        })
        .detach();
    }
}

pub fn render_remote_cover<V: 'static>(
    url: Option<&str>,
    width_px: f32,
    height_px: f32,
    rounded_px: f32,
    cx: &Context<V>,
) -> AnyElement {
    let Some(url) = url.filter(|u| !u.trim().is_empty()) else {
        return div()
            .w(px(width_px))
            .h(px(height_px))
            .rounded(px(rounded_px))
            .bg(theme::BG_CANVAS)
            .border_1()
            .border_color(BORDER_HAIRLINE)
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .child(themed_icon(
                icon!(music),
                (width_px * 0.3).clamp(14.0, 48.0),
                TEXT_SECONDARY.into(),
            ))
            .into_any_element();
    };

    if let Some(png_bytes) = get_cached(url) {
        img(EncodedImageBytes::new(ImageFormat::Png, png_bytes.clone()))
            .w(px(width_px))
            .h(px(height_px))
            .rounded(px(rounded_px))
            .object_fit(ObjectFit::Cover)
            .flex_none()
            .into_any_element()
    } else {
        request_remote_image(url, cx.weak_entity(), cx);
        div()
            .w(px(width_px))
            .h(px(height_px))
            .rounded(px(rounded_px))
            .bg(theme::BG_CANVAS)
            .border_1()
            .border_color(BORDER_HAIRLINE)
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .child(themed_icon(
                icon!(music),
                (width_px * 0.3).clamp(14.0, 48.0),
                TEXT_SECONDARY.into(),
            ))
            .into_any_element()
    }
}

/// `render_remote_cover` twin for item closures that only receive `&mut App` (e.g.
/// `uniform_list` items): wakes `sink` instead of a render context.
pub fn render_remote_cover_with_sink<V: 'static>(
    url: Option<&str>,
    width_px: f32,
    height_px: f32,
    rounded_px: f32,
    sink: &gpui::WeakEntity<V>,
    cx: &mut App,
) -> AnyElement {
    let placeholder = || {
        div()
            .w(px(width_px))
            .h(px(height_px))
            .rounded(px(rounded_px))
            .bg(theme::BG_CANVAS)
            .border_1()
            .border_color(BORDER_HAIRLINE)
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .child(themed_icon(
                icon!(music),
                (width_px * 0.3).clamp(14.0, 48.0),
                TEXT_SECONDARY.into(),
            ))
            .into_any_element()
    };

    let Some(url) = url.filter(|u| !u.trim().is_empty()) else {
        return placeholder();
    };

    if let Some(png_bytes) = get_cached(url) {
        img(EncodedImageBytes::new(ImageFormat::Png, png_bytes.clone()))
            .w(px(width_px))
            .h(px(height_px))
            .rounded(px(rounded_px))
            .object_fit(ObjectFit::Cover)
            .flex_none()
            .into_any_element()
    } else {
        request_remote_image(url, sink.clone(), cx);
        placeholder()
    }
}

/// Full-bleed cover for absolutely-positioned card backgrounds (`inset_0` containers).
///
/// The image and its placeholder both stretch to the container, so fluid-width cards never
/// expose an uncovered strip the overlay scrims would paint over.
pub fn render_remote_cover_fill<V: 'static>(
    url: Option<&str>,
    rounded_px: f32,
    cx: &Context<V>,
) -> AnyElement {
    let placeholder = |icon_size: f32| {
        div()
            .size_full()
            .rounded(px(rounded_px))
            .bg(theme::BG_CANVAS)
            .flex()
            .items_center()
            .justify_center()
            .child(themed_icon(icon!(music), icon_size, TEXT_SECONDARY.into()))
            .into_any_element()
    };

    let Some(url) = url.filter(|u| !u.trim().is_empty()) else {
        return placeholder(48.0);
    };

    if let Some(png_bytes) = get_cached(url) {
        img(EncodedImageBytes::new(ImageFormat::Png, png_bytes.clone()))
            .absolute()
            .inset_0()
            .size_full()
            .rounded(px(rounded_px))
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        request_remote_image(url, cx.weak_entity(), cx);
        placeholder(48.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_cache_evicts_oldest_beyond_cap() {
        let mut cache = MemCache::default();
        for i in 0..(MEM_CACHE_MAX_ENTRIES + 8) {
            cache.insert(format!("u{i}"), Arc::from(&b"x"[..]));
        }
        assert_eq!(cache.map.len(), MEM_CACHE_MAX_ENTRIES);
        assert!(!cache.map.contains_key("u0"));
        assert!(
            cache
                .map
                .contains_key(&format!("u{}", MEM_CACHE_MAX_ENTRIES + 7))
        );

        // 重复插入既有键不应在驱逐序中产生重复条目。
        cache.insert("u1".into(), Arc::from(&b"y"[..]));
        assert_eq!(
            cache
                .order
                .iter()
                .filter(|url| url.as_str() == "u1")
                .count(),
            1
        );
        assert_eq!(cache.order.len(), MEM_CACHE_MAX_ENTRIES);
    }
}
