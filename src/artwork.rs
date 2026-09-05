use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    io::Cursor,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use image::{DynamicImage, GenericImageView, ImageFormat};
use lofty::prelude::TaggedFileExt;

use crate::model::Track;

#[derive(Clone, Debug, PartialEq)]
pub struct ArtworkPalette {
    pub dominant_rgb: [u8; 3],
    pub secondary_rgb: [u8; 3],
    pub dark_ambient_rgb: [u8; 3],
    pub brightness: f32,
    pub mask_alpha: f32,
}

impl Default for ArtworkPalette {
    fn default() -> Self {
        Self {
            dominant_rgb: [220, 40, 60],
            secondary_rgb: [180, 30, 90],
            dark_ambient_rgb: [14, 15, 24],
            brightness: 0.35,
            mask_alpha: 0.60,
        }
    }
}

impl ArtworkPalette {
    pub fn to_serialized_string(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{},{},{:.4},{:.4}",
            self.dominant_rgb[0],
            self.dominant_rgb[1],
            self.dominant_rgb[2],
            self.secondary_rgb[0],
            self.secondary_rgb[1],
            self.secondary_rgb[2],
            self.dark_ambient_rgb[0],
            self.dark_ambient_rgb[1],
            self.dark_ambient_rgb[2],
            self.brightness,
            self.mask_alpha
        )
    }

    pub fn parse_serialized(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.trim().split(',').collect();
        if parts.len() < 11 {
            return None;
        }
        Some(Self {
            dominant_rgb: [
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
            ],
            secondary_rgb: [
                parts[3].parse().ok()?,
                parts[4].parse().ok()?,
                parts[5].parse().ok()?,
            ],
            dark_ambient_rgb: [
                parts[6].parse().ok()?,
                parts[7].parse().ok()?,
                parts[8].parse().ok()?,
            ],
            brightness: parts[9].parse().ok()?,
            mask_alpha: parts[10].parse().ok()?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Artwork {
    pub png: Vec<u8>,
    pub blurred_png: Vec<u8>,
    pub palette: ArtworkPalette,
}

#[derive(Clone, Debug)]
pub struct ArtworkCache {
    directory: PathBuf,
}

impl ArtworkCache {
    pub fn new(directory: PathBuf) -> Result<Self> {
        fs::create_dir_all(&directory)
            .with_context(|| format!("创建封面缓存目录失败: {}", directory.display()))?;
        Ok(Self { directory })
    }

    pub fn load(&self, track: &Track) -> Result<Option<Artwork>> {
        let key = track
            .artwork_key
            .clone()
            .unwrap_or_else(|| track.path.to_string_lossy().into_owned());
        let hash = stable_hash(&key);
        let cache_path = self.directory.join(format!("{:016x}.png", hash));
        let blur_cache_path = self.directory.join(format!("{:016x}_ambient_v2.png", hash));
        let palette_cache_path = self.directory.join(format!("{:016x}_palette.txt", hash));

        if cache_path.exists() {
            let png = fs::read(&cache_path)
                .with_context(|| format!("读取封面缓存失败: {}", cache_path.display()))?;

            let blurred_png = if blur_cache_path.exists() {
                fs::read(&blur_cache_path).ok()
            } else {
                None
            };

            let palette = if palette_cache_path.exists() {
                fs::read_to_string(&palette_cache_path)
                    .ok()
                    .and_then(|s| ArtworkPalette::parse_serialized(&s))
            } else {
                None
            };

            if let (Some(b_png), Some(pal)) = (blurred_png, palette) {
                return Ok(Some(Artwork {
                    png,
                    blurred_png: b_png,
                    palette: pal,
                }));
            }

            // 补全缺失的模糊图与调色板
            if let Ok(img) = image::load_from_memory(&png) {
                let blurred = generate_blurred_artwork(&img).unwrap_or_else(|_| png.clone());
                let pal = extract_palette(&img);
                let _ = fs::write(&blur_cache_path, &blurred);
                let _ = fs::write(&palette_cache_path, pal.to_serialized_string());
                return Ok(Some(Artwork {
                    png,
                    blurred_png: blurred,
                    palette: pal,
                }));
            }

            let fallback_blur = png.clone();
            let fallback_pal = ArtworkPalette::default();
            return Ok(Some(Artwork {
                png,
                blurred_png: fallback_blur,
                palette: fallback_pal,
            }));
        }

        let source = embedded_artwork(&track.path).or_else(|| sidecar_artwork(&track.path));
        let Some(source) = source else {
            return Ok(None);
        };
        let image = image::load_from_memory(&source).context("封面格式不受支持或文件已损坏")?;
        let image = image.thumbnail(image.width().min(768), image.height().min(768));
        let png = encode_png(&image)?;
        let blurred_png = generate_blurred_artwork(&image).unwrap_or_else(|_| png.clone());
        let palette = extract_palette(&image);

        let _ = fs::write(&cache_path, &png);
        let _ = fs::write(&blur_cache_path, &blurred_png);
        let _ = fs::write(&palette_cache_path, palette.to_serialized_string());

        Ok(Some(Artwork {
            png,
            blurred_png,
            palette,
        }))
    }

    pub fn store(&self, key: &str, source: &[u8]) -> Result<Artwork> {
        let image = image::load_from_memory(source).context("在线封面格式不受支持或数据已损坏")?;
        let image = image.thumbnail(image.width().min(768), image.height().min(768));
        let png = encode_png(&image)?;
        let blurred_png = generate_blurred_artwork(&image).unwrap_or_else(|_| png.clone());
        let palette = extract_palette(&image);

        let hash = stable_hash(key);
        let cache_path = self.directory.join(format!("{:016x}.png", hash));
        let blur_cache_path = self.directory.join(format!("{:016x}_ambient_v2.png", hash));
        let palette_cache_path = self.directory.join(format!("{:016x}_palette.txt", hash));

        fs::write(&cache_path, &png)
            .with_context(|| format!("写入封面缓存失败: {}", cache_path.display()))?;
        let _ = fs::write(&blur_cache_path, &blurred_png);
        let _ = fs::write(&palette_cache_path, palette.to_serialized_string());

        Ok(Artwork {
            png,
            blurred_png,
            palette,
        })
    }
}

/// 生成舞台背景专用的超低频色场纹理。
///
/// 纹理只在封面进入缓存时生成一次；运行时仅由 GPU 对该小纹理进行双线性放大和
/// compositor transform，不再对全屏图层执行实时 Gaussian blur。
pub fn generate_blurred_artwork(image: &DynamicImage) -> Result<Vec<u8>> {
    let small = image.resize_exact(48, 48, image::imageops::FilterType::Triangle);
    let blurred = small.blur(12.0);
    encode_png(&blurred)
}

/// 从封面提取主色调与明暗自适应参数
pub fn extract_palette(image: &DynamicImage) -> ArtworkPalette {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return ArtworkPalette::default();
    }

    // 采样网格：步长控制在约 32x32 点，既快又准确
    let step_x = (width / 32).max(1);
    let step_y = (height / 32).max(1);

    let mut total_lum = 0.0f32;
    let mut count = 0usize;

    struct Sample {
        rgb: [u8; 3],
        h: f32,
        s: f32,
        l: f32,
        score: f32,
    }

    let mut samples = Vec::with_capacity(1024);

    for y in (0..height).step_by(step_y as usize) {
        for x in (0..width).step_by(step_x as usize) {
            let pixel = image.get_pixel(x, y);
            // 忽略完全透明像素
            if pixel[3] < 128 {
                continue;
            }
            let r = pixel[0] as f32 / 255.0;
            let g = pixel[1] as f32 / 255.0;
            let b = pixel[2] as f32 / 255.0;

            // ITU-R BT.709 感知明度
            let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            total_lum += lum;
            count += 1;

            let (h, s, l) = rgb_to_hsl(r, g, b);

            // 过滤极黑极白与极低饱和度孤立噪点
            if l < 0.08 || l > 0.94 {
                continue;
            }

            // 鲜艳度打分：偏好中等明度 (0.3~0.7) 与较高饱和度的色彩
            let score = s * 2.0 + (1.0 - (l - 0.5).abs() * 2.0) * 1.2;
            samples.push(Sample {
                rgb: [pixel[0], pixel[1], pixel[2]],
                h,
                s,
                l,
                score,
            });
        }
    }

    let brightness = if count > 0 {
        total_lum / count as f32
    } else {
        0.35
    };

    // 明暗自适应遮罩：整体偏亮时加深遮罩 (0.70~0.78)，偏暗时减少遮罩 (0.45~0.52)
    let mask_alpha = if brightness > 0.65 {
        (0.68 + (brightness - 0.65) * 0.40).clamp(0.68, 0.78)
    } else if brightness < 0.30 {
        (0.46 + (brightness - 0.10) * 0.20).clamp(0.45, 0.52)
    } else {
        0.52 + (brightness - 0.30) * 0.45
    };

    if samples.is_empty() {
        return ArtworkPalette {
            dominant_rgb: [220, 50, 70],
            secondary_rgb: [170, 40, 100],
            dark_ambient_rgb: [15, 14, 22],
            brightness,
            mask_alpha,
        };
    }

    // 按得分降序排序，提取最优色彩
    samples.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let dominant = &samples[0];
    let dominant_rgb = dominant.rgb;

    // 寻找在色相上有适度区分度的辅助色
    let secondary = samples.iter().find(|s| {
        let diff = (s.h - dominant.h).abs();
        let hue_diff = if diff > 180.0 { 360.0 - diff } else { diff };
        hue_diff > 35.0 && (s.l - dominant.l).abs() > 0.08
    });

    let secondary_rgb = secondary.map_or_else(
        || {
            // 色相平移 40° 作为互补辅助色
            hsl_to_rgb(
                (dominant.h + 40.0) % 360.0,
                (dominant.s * 0.9).min(1.0),
                (dominant.l * 1.1).min(0.85),
            )
        },
        |s| s.rgb,
    );

    // 提取深邃底色（以主色调色相为准，饱和度 0.35，明度 0.08）
    let dark_ambient_rgb = hsl_to_rgb(dominant.h, 0.35, 0.08);

    ArtworkPalette {
        dominant_rgb,
        secondary_rgb,
        dark_ambient_rgb,
        brightness,
        mask_alpha,
    }
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let l = (max + min) / 2.0;

    if delta.abs() < 1e-4 {
        return (0.0, 0.0, l);
    }

    let s = if l > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };

    let h = if (max - r).abs() < 1e-4 {
        let mut val = 60.0 * ((g - b) / delta % 6.0);
        if val < 0.0 {
            val += 360.0;
        }
        val
    } else if (max - g).abs() < 1e-4 {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };

    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;

    let (r1, g1, b1) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    [
        ((r1 + m).clamp(0.0, 1.0) * 255.0).round() as u8,
        ((g1 + m).clamp(0.0, 1.0) * 255.0).round() as u8,
        ((b1 + m).clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn embedded_artwork(path: &Path) -> Option<Vec<u8>> {
    let tagged = lofty::read_from_path(path).ok()?;
    tagged.tags().iter().find_map(|tag| {
        tag.pictures()
            .iter()
            .find(|picture| !picture.data().is_empty())
            .map(|picture| picture.data().to_vec())
    })
}

fn sidecar_artwork(path: &Path) -> Option<Vec<u8>> {
    let parent = path.parent()?;
    ["cover", "folder", "front"]
        .into_iter()
        .flat_map(|name| {
            ["png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff", "ico"]
                .into_iter()
                .map(move |extension| parent.join(format!("{name}.{extension}")))
        })
        .find_map(|candidate| fs::read(candidate).ok())
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>> {
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, ImageFormat::Png)
        .context("编码封面缩略图失败")?;
    Ok(output.into_inner())
}

fn stable_hash(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::{env, fs, time::SystemTime};

    use image::{DynamicImage, GenericImageView, ImageBuffer, Rgba};

    use super::*;

    #[test]
    fn sidecar_artwork_is_decoded_and_cached_as_png() {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = env::temp_dir().join(format!("yinqidao-artwork-{suffix}"));
        fs::create_dir_all(&root).expect("root");
        let image =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(12, 8, Rgba([30, 90, 180, 255])));
        image.save(root.join("cover.png")).expect("cover");
        let track = Track {
            id: 1,
            path: root.join("song.mp3"),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            year: None,
            genre: None,
            duration_ms: 1_000,
            codec: "MP3".into(),
            sample_rate: 44_100,
            channels: 2,
            artwork_key: None,
        };
        let cache = ArtworkCache::new(root.join("cache")).expect("cache");
        let artwork = cache.load(&track).expect("load").expect("artwork");
        assert_eq!(
            image::load_from_memory(&artwork.png)
                .expect("decode cached artwork")
                .dimensions(),
            (12, 8)
        );
        assert!(artwork.png.starts_with(b"\x89PNG"));
        assert!(artwork.blurred_png.starts_with(b"\x89PNG"));
        assert!(artwork.palette.mask_alpha >= 0.40 && artwork.palette.mask_alpha <= 0.85);
        assert!(cache.load(&track).expect("cached").is_some());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn test_extract_palette_brightness_and_mask_adaptation() {
        // 测试亮色封面：亮度高时遮罩自动加深以保证可读性
        let bright_img =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(32, 32, Rgba([240, 240, 240, 255])));
        let bright_pal = extract_palette(&bright_img);
        assert!(bright_pal.brightness > 0.8);
        assert!(bright_pal.mask_alpha >= 0.70);

        // 测试暗色封面：亮度低时遮罩自适应放宽以保留色彩景深
        let dark_img =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(32, 32, Rgba([20, 20, 25, 255])));
        let dark_pal = extract_palette(&dark_img);
        assert!(dark_pal.brightness < 0.2);
        assert!(dark_pal.mask_alpha <= 0.55);

        // 测试鲜艳彩色封面
        let color_img =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(32, 32, Rgba([220, 50, 80, 255])));
        let color_pal = extract_palette(&color_img);
        assert_eq!(color_pal.dominant_rgb, [220, 50, 80]);
        let serialized = color_pal.to_serialized_string();
        let parsed =
            ArtworkPalette::parse_serialized(&serialized).expect("parse serialized palette");
        assert_eq!(parsed.dominant_rgb, color_pal.dominant_rgb);
    }
}
