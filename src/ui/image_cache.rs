use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Cursor,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::Duration,
};

use gpui::{
    AnyElement, Context, EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, Styled, div, img,
    prelude::*, px,
};
use lucide_gpui::icon;

use super::theme::{self, ACCENT_RED, BORDER_HAIRLINE, TEXT_SECONDARY, themed_icon};

static MEM_CACHE: OnceLock<RwLock<HashMap<String, Arc<Vec<u8>>>>> = OnceLock::new();
static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static DISK_DIR: OnceLock<PathBuf> = OnceLock::new();

fn mem_cache() -> &'static RwLock<HashMap<String, Arc<Vec<u8>>>> {
    MEM_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
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

pub fn get_cached(url: &str) -> Option<Arc<Vec<u8>>> {
    if let Ok(guard) = mem_cache().read() {
        if let Some(bytes) = guard.get(url) {
            return Some(bytes.clone());
        }
    }

    if let Some(disk_dir) = DISK_DIR.get() {
        let file_path = disk_dir.join(url_disk_filename(url));
        if file_path.is_file() {
            if let Ok(bytes) = fs::read(&file_path) {
                if bytes.starts_with(b"\x89PNG") {
                    let arc = Arc::new(bytes);
                    if let Ok(mut guard) = mem_cache().write() {
                        guard.insert(url.to_string(), arc.clone());
                    }
                    return Some(arc);
                }
            }
        }
    }

    None
}

pub fn load_remote_image<V: 'static>(url: &str, cx: &mut Context<V>) -> Option<Arc<Vec<u8>>> {
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
            let res = fetch_and_normalize(&fetch_url).await;
            Ok((fetch_url, res))
        });

        cx.spawn(async move |this, cx| {
            if let Ok((url_done, Some(png_bytes))) = task.await {
                let arc = Arc::new(png_bytes);
                if let Some(disk_dir) = DISK_DIR.get() {
                    let path = disk_dir.join(url_disk_filename(&url_done));
                    let _ = fs::write(path, arc.as_slice());
                }
                if let Ok(mut guard) = mem_cache().write() {
                    guard.insert(url_done.clone(), arc);
                }
                if let Ok(mut set) = in_flight().lock() {
                    set.remove(&url_done);
                }
                let _ = this.update(cx, |_, cx| {
                    cx.notify();
                });
            } else if let Ok(mut set) = in_flight().lock() {
                set.remove(&target_url);
            }
        })
        .detach();
    }

    None
}

async fn fetch_and_normalize(url: &str) -> Option<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;

    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.bytes().await.ok()?;

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

pub fn render_remote_avatar<V: 'static>(
    url: Option<&str>,
    size_px: f32,
    is_default: bool,
    cx: &mut Context<V>,
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
        img(EncodedImageBytes::new(
            ImageFormat::Png,
            (*png_bytes).clone(),
        ))
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
        if let Ok(handle) = crate::runtime::io_handle() {
            handle.spawn(async move {
                let result = fetch_and_normalize(&fetch_url).await;
                if let Some(png_bytes) = result {
                    let arc = Arc::new(png_bytes);
                    if let Some(disk_dir) = DISK_DIR.get() {
                        let path = disk_dir.join(url_disk_filename(&fetch_url));
                        let _ = fs::write(path, arc.as_slice());
                    }
                    if let Ok(mut guard) = mem_cache().write() {
                        guard.insert(fetch_url.clone(), arc);
                    }
                }
                if let Ok(mut set) = in_flight().lock() {
                    set.remove(&fetch_url);
                }
            });
        }
    }
}

pub fn prefetch_urls<V: 'static>(urls: Vec<String>, cx: &mut Context<V>) {
    let urls_to_fetch: Vec<String> = urls
        .into_iter()
        .filter(|u| !u.trim().is_empty() && get_cached(u).is_none())
        .collect();
    if urls_to_fetch.is_empty() {
        return;
    }

    let task = gpui_tokio::Tokio::spawn_result(cx, async move {
        for url in urls_to_fetch {
            let should_fetch = {
                let Ok(mut set) = in_flight().lock() else {
                    continue;
                };
                set.insert(url.clone())
            };
            if should_fetch {
                if let Some(png_bytes) = fetch_and_normalize(&url).await {
                    let arc = Arc::new(png_bytes);
                    if let Some(disk_dir) = DISK_DIR.get() {
                        let path = disk_dir.join(url_disk_filename(&url));
                        let _ = fs::write(path, arc.as_slice());
                    }
                    if let Ok(mut guard) = mem_cache().write() {
                        guard.insert(url.clone(), arc);
                    }
                }
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

pub fn render_remote_cover(
    url: Option<&str>,
    width_px: f32,
    height_px: f32,
    rounded_px: f32,
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
        img(EncodedImageBytes::new(
            ImageFormat::Png,
            (*png_bytes).clone(),
        ))
        .w(px(width_px))
        .h(px(height_px))
        .rounded(px(rounded_px))
        .object_fit(ObjectFit::Cover)
        .flex_none()
        .into_any_element()
    } else {
        fetch_detached(url);
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
