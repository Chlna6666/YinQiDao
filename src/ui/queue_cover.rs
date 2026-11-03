//! Album-cover interaction shared by local and online playback queues.
//! Animation remains on the compositor; virtualized rows do not poll playback on a timer.
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, AnyElement, Easing, ElementId,
    IntoElement, SharedString, div, prelude::*, px, radians,
};
use lucide_gpui::icon;

use super::theme::{self, dark, themed_icon};
use crate::model::PlaybackState;

const COVER_SIZE: f32 = 42.0;

/// The artwork is always visible beneath the interaction state. Only an active or hovered
/// item gets the darkened overlay, so empty and non-hovered rows stay visually calm.
pub(super) fn tile(
    artwork: AnyElement,
    current: bool,
    playback: PlaybackState,
    buffering: bool,
    hovered: bool,
    animation_key: u64,
) -> AnyElement {
    let animated = buffering || (current && (hovered || playback == PlaybackState::Playing));
    let glyph = if buffering {
        icon!(loader_circle)
    } else if current && hovered {
        icon!(rotate_cw)
    } else if current {
        match playback {
            PlaybackState::Playing => icon!(audio_lines),
            PlaybackState::Paused => icon!(pause),
            PlaybackState::Stopped | PlaybackState::Error => icon!(square),
            PlaybackState::Loading | PlaybackState::Buffering => icon!(loader_circle),
        }
    } else {
        icon!(play)
    };

    let glyph = div()
        .size(px(24.0))
        .transform_origin(gpui::TransformOrigin::new(0.5, 0.5))
        .flex()
        .items_center()
        .justify_center()
        .child(themed_icon(glyph, 17.0, dark::white(1.0)));

    let glyph = if animated {
        let spec = AnimationSpec::new(Duration::from_millis(if buffering { 1000 } else { 1250 }))
            .ease(Easing::Linear)
            .repeat(gpui::RepeatMode::Forever);
        let spec = if buffering || hovered {
            spec
        } else {
            spec.direction(gpui::AnimationDirection::Alternate)
        };
        let property = if buffering || hovered {
            AnimationProperty::rotation(radians(0.0), radians(std::f32::consts::TAU))
        } else {
            AnimationProperty::opacity(0.52, 1.0)
        };
        glyph
            .with_animation(
                ElementId::NamedInteger(SharedString::new_static("queue-cover-svg"), animation_key),
                Animation::from_spec(spec).with_property(property),
                |element, _| element,
            )
            .into_any_element()
    } else {
        glyph.into_any_element()
    };

    div()
        .relative()
        .size(px(COVER_SIZE))
        .flex_none()
        .rounded(px(9.0))
        .overflow_hidden()
        .child(artwork)
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(px(9.0))
                .bg(dark::scrim(if current { 0.38 } else { 0.52 }))
                .opacity(if current || hovered || buffering {
                    1.0
                } else {
                    0.0
                })
                .transition(theme::hover_transition())
                .flex()
                .items_center()
                .justify_center()
                .child(glyph),
        )
        .into_any_element()
}
