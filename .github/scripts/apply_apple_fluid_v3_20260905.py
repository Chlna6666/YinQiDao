from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, got {count}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


artwork = Path("src/artwork.rs")
player = Path("src/ui/player_stage.rs")

# Bump the generated ambient texture cache so existing overly-sharp/old blur files cannot mask
# this fix. There are two construction sites: load() and store().
text = artwork.read_text(encoding="utf-8")
old_cache = 'format!("{:016x}_blur.png", hash)'
count = text.count(old_cache)
if count != 2:
    raise SystemExit(f"src/artwork.rs: expected two blur cache paths, got {count}")
text = text.replace(old_cache, 'format!("{:016x}_ambient_v2.png", hash)')
artwork.write_text(text, encoding="utf-8")

replace_once(
    str(artwork),
    '''/// 降采样与高斯多级模糊处理（先降采样至 64x64 再模糊，兼顾极致性能与双线性抗锯齿）
pub fn generate_blurred_artwork(image: &DynamicImage) -> Result<Vec<u8>> {
    let small = image.resize_exact(64, 64, image::imageops::FilterType::Triangle);
    let blurred = small.blur(10.0);
    encode_png(&blurred)
}''',
    '''/// 生成舞台背景专用的超低频色场纹理。
///
/// 纹理只在封面进入缓存时生成一次；运行时仅由 GPU 对该小纹理进行双线性放大和
/// compositor transform，不再对全屏图层执行实时 Gaussian blur。
pub fn generate_blurred_artwork(image: &DynamicImage) -> Result<Vec<u8>> {
    let small = image.resize_exact(48, 48, image::imageops::FilterType::Triangle);
    let blurred = small.blur(12.0);
    encode_png(&blurred)
}''',
)

text = player.read_text(encoding="utf-8")
start = text.index("fn ambient_background(")
end = text.index("fn ambient_palette(", start)
new_background = r'''fn ambient_background(
    id: Option<i64>,
    artwork: Option<Arc<[u8]>>,
    blurred: Option<Arc<[u8]>>,
    palette: Option<&ArtworkPalette>,
    dynamic: bool,
    _blur_radius: f32,
) -> impl IntoElement {
    let id = id.unwrap_or_default();
    let (c1, c2, c3, dark, mask) = ambient_palette(id, palette);
    let mut root = div().absolute().inset_0().overflow_hidden().bg(dark);

    if let Some(bytes) = blurred.or(artwork) {
        // Stable base: the 48x48 ambient texture is already heavily blurred off the render path.
        // Physical overscan (rather than a style scale transform) guarantees that no compositor
        // clip edge can enter the viewport.
        root = root.child(
            div()
                .absolute()
                .left(relative(-0.24))
                .top(relative(-0.30))
                .w(relative(1.48))
                .h(relative(1.60))
                .opacity(0.52)
                .child(
                    img(EncodedImageBytes::new(ImageFormat::Png, bytes.clone()))
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                )
                .composite_layer(),
        );

        // The two moving layers contain no blur/filter at all. Each animation owns exactly one
        // Translation property whose vector contains both X and Y, so GPUI can keep the entire
        // motion on the retained compositor path without creating per-frame offscreen surfaces.
        root = root
            .child(ambient_texture_motion(
                "stage-ambient-primary",
                bytes.clone(),
                -0.34,
                -0.42,
                1.68,
                1.84,
                0.30,
                54.0,
                34.0,
                24,
                false,
                dynamic,
            ))
            .child(ambient_texture_motion(
                "stage-ambient-secondary",
                bytes,
                -0.46,
                -0.52,
                1.92,
                2.04,
                0.18,
                42.0,
                -58.0,
                31,
                true,
                dynamic,
            ));
    }

    // Full-screen gradients have no local layer bounds. GPUI's helper supports two stops, so the
    // third palette colour is composed as another full-screen gradient instead of a local blob.
    root
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(linear_gradient(
                    128.0,
                    linear_color_stop(c1.opacity(0.18), 0.0),
                    linear_color_stop(c2.opacity(0.12), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(linear_gradient(
                    306.0,
                    linear_color_stop(c3.opacity(0.13), 0.0),
                    linear_color_stop(c1.opacity(0.025), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(linear_gradient(
                    180.0,
                    linear_color_stop(
                        hsla(0.0, 0.0, 0.008, (mask * 0.18).clamp(0.06, 0.16)),
                        0.0,
                    ),
                    linear_color_stop(
                        hsla(0.0, 0.0, 0.002, (mask * 0.48).clamp(0.20, 0.40)),
                        1.0,
                    ),
                )),
        )
}

#[allow(clippy::too_many_arguments)]
fn ambient_texture_motion(
    animation_id: &'static str,
    bytes: Arc<[u8]>,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    opacity: f32,
    drift_x: f32,
    drift_y: f32,
    period_seconds: u64,
    reverse: bool,
    animate: bool,
) -> gpui::AnyElement {
    let layer = div()
        .absolute()
        .left(relative(left))
        .top(relative(top))
        .w(relative(width))
        .h(relative(height))
        .opacity(opacity)
        .child(
            img(EncodedImageBytes::new(ImageFormat::Png, bytes))
                .size_full()
                .object_fit(ObjectFit::Cover),
        )
        .composite_layer();

    if !animate {
        return layer.into_any_element();
    }

    let direction = if reverse {
        AnimationDirection::AlternateReverse
    } else {
        AnimationDirection::Alternate
    };
    let motion = Animation::from_spec(
        AnimationSpec::new(Duration::from_secs(period_seconds))
            .repeat(RepeatMode::Forever)
            .direction(direction)
            .ease(Easing::InOutCubic),
    )
    .with_property(AnimationProperty::translation(
        point(px(-drift_x), px(-drift_y)),
        point(px(drift_x), px(drift_y)),
    ));

    layer
        .with_animation(
            SharedString::from(animation_id),
            motion,
            |element, _| element,
        )
        .into_any_element()
}

'''
player.write_text(text[:start] + new_background + text[end:], encoding="utf-8")
