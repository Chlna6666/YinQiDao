use std::{f32::consts::FRAC_PI_2, time::Duration};

use crate::{
    audio::decoder::{DecodeError, DecoderStream},
    model::Track,
};

const ANALYSIS_WINDOW_MS: u64 = 50;
const MAX_ANALYSIS_MS: u64 = 12_000;
const MAX_ANALYSIS_WINDOWS: usize = (MAX_ANALYSIS_MS / ANALYSIS_WINDOW_MS) as usize + 1;
const STABLE_WINDOWS: usize = 5;
const FLOOR_RMS: f64 = 0.0025;
const ACTIVE_RMS: f64 = 0.0063;
const PEAK_RMS: f64 = 0.0120;
const MIN_USEFUL_CUE_MS: u64 = 150;
const MIN_CUE_CONFIDENCE: f32 = 0.62;

std::thread_local! {
    // Smart Cue runs on the dedicated preloader thread. Keep its decoder PCM capacity attached to
    // that thread so changing tracks clears length but does not repeatedly return/reacquire the same
    // medium-sized heap block from the allocator.
    static SMART_CUE_PCM_SCRATCH: std::cell::RefCell<Vec<f32>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SmartCue {
    pub position: Duration,
    pub confidence: f32,
}

/// Analyze only. The caller must prepare a fresh decoder at the returned PCM position.
/// Keeping analysis and playback positioning separate avoids relying on container seek accuracy.
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
        return Ok(SmartCue::default());
    }

    let validation_tail_ms = ANALYSIS_WINDOW_MS * (STABLE_WINDOWS as u64 + 2);
    let scan_ms = hard_cap_ms
        .saturating_add(validation_tail_ms)
        .min(MAX_ANALYSIS_MS)
        .min((track.duration_ms / 4).max(ANALYSIS_WINDOW_MS));
    let initial_rate = track.sample_rate.max(1);
    let initial_channels = track.channels.max(1);
    let initial_chunk_samples = ((u64::from(initial_rate)
        .saturating_mul(u64::from(initial_channels))
        .saturating_mul(ANALYSIS_WINDOW_MS))
        / 1_000)
        .max(1)
        .min(262_144) as usize;

    SMART_CUE_PCM_SCRATCH.with_borrow_mut(|chunk_samples| {
        chunk_samples.clear();
        if chunk_samples.capacity() < initial_chunk_samples {
            chunk_samples.reserve(initial_chunk_samples);
        }

        let mut current_rate = initial_rate;
        let mut current_channels = initial_channels;
        // Analysis is hard-bounded to 12 seconds, so keep the 50 ms RMS windows on the preloader
        // thread's stack instead of repeatedly growing and freeing a short-lived heap Vec.
        let mut windows = [(0_u64, 0.0_f64); MAX_ANALYSIS_WINDOWS];
        let mut window_count = 0_usize;
        let mut sum_squares = 0.0_f64;
        let mut sample_count = 0_u64;
        let mut window_start_ms = 0_u64;

        while decoder.position().as_millis() as u64 <= scan_ms {
            let Some((sample_rate, channels)) = decoder.next_chunk_into(chunk_samples)? else {
                break;
            };
            current_rate = sample_rate.max(1);
            current_channels = channels.max(1);
            let samples_per_window =
                ((u64::from(current_rate) * u64::from(current_channels) * ANALYSIS_WINDOW_MS)
                    / 1_000)
                    .max(1);

            for sample in chunk_samples.iter() {
                let value = f64::from(*sample);
                sum_squares += value * value;
                sample_count += 1;
                if sample_count >= samples_per_window {
                    if window_count >= windows.len() {
                        break;
                    }
                    let rms = (sum_squares / sample_count as f64).sqrt();
                    windows[window_count] = (window_start_ms, rms);
                    window_count += 1;
                    window_start_ms = window_start_ms.saturating_add(ANALYSIS_WINDOW_MS);
                    sum_squares = 0.0;
                    sample_count = 0;
                    if window_start_ms >= scan_ms {
                        break;
                    }
                }
            }
            if window_start_ms >= scan_ms || window_count >= windows.len() {
                break;
            }
        }

        if sample_count > 0 && window_count < windows.len() {
            windows[window_count] = (window_start_ms, (sum_squares / sample_count as f64).sqrt());
            window_count += 1;
        }
        let windows = &windows[..window_count];

        let mut best = SmartCue::default();
        for index in 0..windows.len() {
            if index + STABLE_WINDOWS > windows.len() {
                break;
            }
            let stable = &windows[index..index + STABLE_WINDOWS];
            let active_count = stable.iter().filter(|(_, rms)| *rms >= FLOOR_RMS).count();
            let max_rms = stable.iter().map(|(_, rms)| *rms).fold(0.0_f64, f64::max);
            if stable[0].1 < ACTIVE_RMS || active_count < STABLE_WINDOWS - 1 || max_rms < PEAK_RMS {
                continue;
            }

            let onset_ms = stable[0].0;
            if onset_ms > hard_cap_ms {
                break;
            }

            if has_sustained_leading_audio(&windows[..index]) {
                break;
            }

            let cue_ms = onset_ms.saturating_sub(80);
            if cue_ms < MIN_USEFUL_CUE_MS {
                break;
            }
            let energy_confidence = ((max_rms - FLOOR_RMS) / 0.05).clamp(0.0, 1.0) as f32;
            let stability_confidence = active_count as f32 / STABLE_WINDOWS as f32;
            let confidence =
                (0.55 * stability_confidence + 0.45 * energy_confidence).clamp(0.0, 1.0);
            if confidence >= MIN_CUE_CONFIDENCE {
                best = SmartCue {
                    position: Duration::from_millis(cue_ms),
                    confidence,
                };
            }
            break;
        }

        tracing::debug!(
            cue_ms = best.position.as_millis() as u64,
            confidence = best.confidence,
            sample_rate = current_rate,
            channels = current_channels,
            track_id = track.id,
            "下一曲 Smart Cue 分析完成"
        );
        chunk_samples.clear();
        Ok(best)
    })
}

fn has_sustained_leading_audio(windows: &[(u64, f64)]) -> bool {
    windows
        .windows(3)
        .any(|group| group.iter().all(|(_, rms)| *rms >= FLOOR_RMS))
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
    let genre = track.genre.as_deref().unwrap_or_default();
    if contains_any(
        genre,
        &["classical", "ambient", "new age", "古典", "氛围", "新世纪"],
    ) {
        800
    } else if contains_any(
        genre,
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
    needles
        .iter()
        .any(|needle| contains_keyword(haystack, needle))
}

fn contains_keyword(haystack: &str, needle: &str) -> bool {
    if needle.is_ascii() {
        let needle = needle.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle));
    }
    haystack.contains(needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_curves_are_monotonic_at_halfway() {
        let out = fade_out_gain(0.5);
        let input = fade_in_gain(0.5);
        assert!((out - input).abs() < 1.0e-6);
        assert!(out > 0.7 && out < 0.71);
    }

    #[test]
    fn sustained_quiet_intro_is_not_treated_as_leading_silence() {
        let windows = [
            (0, FLOOR_RMS + 0.0002),
            (50, FLOOR_RMS + 0.0003),
            (100, FLOOR_RMS + 0.0001),
            (150, FLOOR_RMS + 0.0004),
        ];
        assert!(has_sustained_leading_audio(&windows));
    }

    #[test]
    fn isolated_encoder_noise_does_not_block_silence_skip() {
        let windows = [
            (0, 0.0002),
            (50, FLOOR_RMS + 0.0002),
            (100, 0.0003),
            (150, 0.0002),
        ];
        assert!(!has_sustained_leading_audio(&windows));
    }

    #[test]
    fn genre_matching_avoids_lowercase_allocation_and_keeps_ascii_case_insensitive() {
        assert!(contains_keyword("Ambient / Vocal", "ambient"));
        assert!(contains_keyword("现代古典", "古典"));
        assert!(!contains_keyword("Rock", "jazz"));
    }
}
