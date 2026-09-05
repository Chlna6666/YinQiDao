from pathlib import Path


path = Path("src/ui/player_stage.rs")
text = path.read_text(encoding="utf-8")
start = text.index("fn ambient_background(")
end = text.index("fn ambient_palette(", start)

new_section = r'''fn ambient_background(
    id: Option<i64>,
    artwork: Option<Arc<[u8]>>,
    blurred: Option<Arc<[u8]>>,
    palette: Option<&ArtworkPalette>,
    dynamic: bool,
    blur_radius: f32,
) -> impl IntoElement {
    let id = id.unwrap_or_default();
    let (c1, c2, c3, dark, mask) = ambient_palette(id, palette);
    let field_blur = (blur_radius * 4.0).clamp(56.0, 96.0);
    let artwork_blur = (blur_radius * 1.15).clamp(14.0, 28.0);
    let mut root = div().absolute().inset_0().overflow_hidden().bg(dark);

    // Keep one oversized, static cover-derived field underneath the moving palette field.
    // The physical overscan is intentional: transform-only scaling kept the original compositor
    // clip bounds and exposed rectangular edges while the layer moved.
    if let Some(bytes) = blurred.or(artwork) {
        root = root.child(
            div()
                .absolute()
                .left(relative(-0.18))
                .top(relative(-0.24))
                .w(relative(1.36))
                .h(relative(1.48))
                .opacity(0.28)
                .blur(px(artwork_blur))
                .child(
                    img(EncodedImageBytes::new(ImageFormat::Png, bytes))
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                )
                .composite_layer(),
        );
    }

    // Apple Music reads as one continuous colour atmosphere. These ellipses are deliberately
    // larger than the viewport and heavily blurred, so their own bounds never become visible.
    // X and Y motion live on separate compositor wrappers: each wrapper owns exactly one scene
    // animation property, preserving the retained fast path while producing a curved 2D orbit.
    root
        .child(fluid_field_blob(
            "stage-fluid-field-a",
            -0.44,
            -0.54,
            1.34,
            1.46,
            c1,
            0.58,
            field_blur,
            19,
            27,
            112.0,
            76.0,
            false,
            dynamic,
        ))
        .child(fluid_field_blob(
            "stage-fluid-field-b",
            0.18,
            -0.46,
            1.28,
            1.32,
            c2,
            0.50,
            field_blur * 1.08,
            23,
            31,
            96.0,
            104.0,
            true,
            dynamic,
        ))
        .child(fluid_field_blob(
            "stage-fluid-field-c",
            -0.38,
            0.20,
            1.46,
            1.34,
            c3,
            0.46,
            field_blur * 1.12,
            29,
            37,
            128.0,
            82.0,
            true,
            dynamic,
        ))
        .child(fluid_field_blob(
            "stage-fluid-field-d",
            0.34,
            0.28,
            1.18,
            1.26,
            c1,
            0.30,
            field_blur * 1.18,
            37,
            43,
            84.0,
            118.0,
            false,
            dynamic,
        ))
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(linear_gradient(
                    180.0,
                    linear_color_stop(
                        hsla(0.0, 0.0, 0.008, (mask * 0.22).clamp(0.08, 0.20)),
                        0.0,
                    ),
                    linear_color_stop(
                        hsla(0.0, 0.0, 0.003, (mask * 0.54).clamp(0.24, 0.46)),
                        1.0,
                    ),
                )),
        )
}

#[allow(clippy::too_many_arguments)]
fn fluid_field_blob(
    animation_id: &'static str,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    color: gpui::Hsla,
    alpha: f32,
    blur: f32,
    x_period_seconds: u64,
    y_period_seconds: u64,
    drift_x: f32,
    drift_y: f32,
    reverse: bool,
    animate: bool,
) -> gpui::AnyElement {
    let blob = div()
        .absolute()
        .left(relative(left))
        .top(relative(top))
        .w(relative(width))
        .h(relative(height))
        .rounded_full()
        .bg(color.opacity(alpha))
        .blur(px(blur))
        .composite_layer();

    if !animate {
        return blob.into_any_element();
    }

    let x_direction = if reverse {
        AnimationDirection::AlternateReverse
    } else {
        AnimationDirection::Alternate
    };
    let y_direction = if reverse {
        AnimationDirection::Alternate
    } else {
        AnimationDirection::AlternateReverse
    };

    let x_motion = Animation::from_spec(
        AnimationSpec::new(Duration::from_secs(x_period_seconds))
            .repeat(RepeatMode::Forever)
            .direction(x_direction)
            .ease(Easing::Linear),
    )
    .with_property(AnimationProperty::translation(
        point(px(-drift_x), px(0.0)),
        point(px(drift_x), px(0.0)),
    ));
    let x_layer = blob.with_animation(
        SharedString::from(format!("{animation_id}-x")),
        x_motion,
        |element, _| element,
    );

    let y_carrier = div()
        .absolute()
        .inset_0()
        .child(x_layer)
        .composite_layer();
    let y_motion = Animation::from_spec(
        AnimationSpec::new(Duration::from_secs(y_period_seconds))
            .repeat(RepeatMode::Forever)
            .direction(y_direction)
            .ease(Easing::Linear),
    )
    .with_property(AnimationProperty::translation(
        point(px(0.0), px(-drift_y)),
        point(px(0.0), px(drift_y)),
    ));

    y_carrier
        .with_animation(
            SharedString::from(format!("{animation_id}-y")),
            y_motion,
            |element, _| element,
        )
        .into_any_element()
}

'''

text = text[:start] + new_section + text[end:]

old_mixed = '''        let mixed = [
            ((palette.dominant_rgb[0] as u16 + palette.secondary_rgb[2] as u16) / 2) as u8,
            ((palette.dominant_rgb[1] as u16 + palette.secondary_rgb[0] as u16) / 2) as u8,
            ((palette.dominant_rgb[2] as u16 + palette.secondary_rgb[1] as u16) / 2) as u8,
        ];'''
new_mixed = '''        let mixed = [
            ((palette.dominant_rgb[0] as u16 + palette.secondary_rgb[0] as u16) / 2) as u8,
            ((palette.dominant_rgb[1] as u16 + palette.secondary_rgb[1] as u16) / 2) as u8,
            ((palette.dominant_rgb[2] as u16 + palette.secondary_rgb[2] as u16) / 2) as u8,
        ];'''
if old_mixed not in text:
    raise SystemExit("ambient palette mixing block no longer matches expected source")
text = text.replace(old_mixed, new_mixed, 1)

path.write_text(text, encoding="utf-8")
