use std::{f32::consts::FRAC_PI_2, time::Duration};

use crate::{
    audio::decoder::{DecodeError, DecoderStream},
    model::Track,
};

const ANALYSIS_WINDOW_MS: u64 = 50;
const MAX_ANALYSIS_MS: u64 = 12_000;
const STABLE_WINDOWS: usize = 5;
const FLOOR_RMS: f64 = 0.0025;
const ACTIVE_RMS: f64 = 0.0063;
const PEAK_RMS: f64 = 0.0120;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SmartCue {
    pub position: Duration,
    pub confidence: f32,
}

pub(crate) fn analyze_smart_cue(
    decoder: &mut DecoderStream,
    track: &Track,
    configured_max_cue_ms: u64,
) -> Result<SmartCue, DecodeError> {
    let style_cap_ms = smart_cue_style_cap_ms(track);
    let hard_cap_ms = configured_max_cue_ms
        .min(style_cap_ms)
        .min(track.duration_ms / 8)
        .min(MAX_ANALYSIS_MS);
    if hard_cap_ms < ANALYSIS_WINDOW_MS {
        decoder.seek(Duration::ZERO)?;
        return Ok(SmartCue::default());
    }

    decoder.seek(Duration::ZERO)?;
    let scan_ms = MAX_ANALYSIS_MS.min(track.duration_ms / 4).max(ANALYSIS_WINDOW_MS);
    let mut chunk_samples = Vec::new();
    let mut windows = Vec::<(u64, f64)>::new();
    let mut sum_squares = 0.0_f64;
    let mut sample_count = 0_u64;
    let mut window_start_ms = 0_u64;
    let mut current_rate = track.sample_rate.max(1);
    let mut current_channels = track.channels.max(1);

    while decoder.position().as_millis() as u64 <= scan_ms {
        let Some((sample_rate, channels)) = decoder.next_chunk_into(&mut chunk_samples)? else {
            break;
        };
        current_rate = sample_rate.max(1);
        current_channels = channels.max(1);
        let samples_per_window = ((u64::from(current_rate) * u64::from(current_channels)
            * ANALYSIS_WINDOW_MS)
            / 1_000)
            .max(1);

        for sample in &chunk_samples {
            let value = f64::from(*sample);
            sum_squares += value * value;
            sample_count += 1;
            if sample_count >= samples_per_window {
                let rms = (sum_squares / sample_count as f64).sqrt();
                windows.push((window_start_ms, rms));
                window_start_ms = window_start_ms.saturating_add(ANALYSIS_WINDOW_MS);
                sum_squares = 0.0;
                sample_count = 0;
                if window_start_ms >= scan_ms {
                    break;
                }
            }
        }
        if window_start_ms >= scan_ms {
            break;
        }
    }

    if sample_count > 0 {
        windows.push((
            window_start_ms,
            (sum_squares / sample_count as f64).sqrt(),
        ));
    }

    let mut best = SmartCue::default();
    for index in 0..windows.len() {
        if index + STABLE_WINDOWS > windows.len() {
            break;
        }
        let stable = &windows[index..index + STABLE_WINDOWS];
        let active_count = stable.iter().filter(|(_, rms)| *rms >= FLOOR_RMS).count();
        let max_rms = stable
            .iter()
            .map(|(_, rms)| *rms)
            .fold(0.0_f64, f64::max);
        if stable[0].1 >= ACTIVE_RMS && active_count >= STABLE_WINDOWS - 1 && max_rms >= PEAK_RMS {
            let onset_ms = stable[0].0;
            if onset_ms <= hard_cap_ms {
                let cue_ms = onset_ms.saturating_sub(80);
                let energy_confidence = ((max_rms - FLOOR_RMS) / 0.05).clamp(0.0, 1.0) as f32;
                let stability_confidence = active_count as f32 / STABLE_WINDOWS as f32;
                best = SmartCue {
                    position: Duration::from_millis(cue_ms),
                    confidence: (0.55 * stability_confidence + 0.45 * energy_confidence)
                        .clamp(0.0, 1.0),
                };
            }
            break;
        }
    }

    decoder.seek(best.position)?;
    tracing::debug!(
        cue_ms = best.position.as_millis() as u64,
        confidence = best.confidence,
        sample_rate = current_rate,
        channels = current_channels,
        track_id = track.id,
        "下一曲 Smart Cue 分析完成"
    );
    Ok(best)
}

pub(crate) fn equal_power_gains(progress: f32) -> (f32, f32) {
    let progress = progress.clamp(0.0, 1.0);
    let angle = progress * FRAC_PI_2;
    (angle.cos(), angle.sin())
}

pub(crate) fn fade_out_gain(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    (progress * FRAC_PI_2).cos()
}

pub(crate) fn fade_in_gain(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    (progress * FRAC_PI_2).sin()
}

fn smart_cue_style_cap_ms(track: &Track) -> u64 {
    let genre = track.genre.as_deref().unwrap_or_default().to_lowercase();
    if contains_any(
        &genre,
        &["classical", "ambient", "new age", "古典", "氛围", "新世纪"],
    ) {
        800
    } else if contains_any(
        &genre,
        &[
            "jazz", "blues", "soul", "folk", "acoustic", "vocal", "爵士", "蓝调", "民谣", "原声",
        ],
    ) {
        1_500
    } else {
        4_000
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_power_curve_keeps_endpoints_exact() {
        assert_eq!(equal_power_gains(0.0), (1.0, 0.0));
        let end = equal_power_gains(1.0);
        assert!(end.0.abs() < 1.0e-6);
        assert!((end.1 - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn fade_curves_are_monotonic_at_halfway() {
        let out = fade_out_gain(0.5);
        let input = fade_in_gain(0.5);
        assert!((out - input).abs() < 1.0e-6);
        assert!(out > 0.7 && out < 0.71);
    }
}
