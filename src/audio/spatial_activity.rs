use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use yinqidao_audio_spatial::{MAX_DEBUG_SOURCES, SourceActivity};

const ACTIVITY_WORDS: usize = 2;
const READ_RETRIES: usize = 4;

static VALID: AtomicBool = AtomicBool::new(false);
static EPOCH: AtomicU64 = AtomicU64::new(0);
static SOURCE_COUNT: AtomicU32 = AtomicU32::new(0);
static ACTIVITY: [AtomicU32; MAX_DEBUG_SOURCES * ACTIVITY_WORDS] =
    [const { AtomicU32::new(0) }; MAX_DEBUG_SOURCES * ACTIVITY_WORDS];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpatialSourceActivitySnapshot {
    pub source_count: usize,
    pub sources: [SourceActivity; MAX_DEBUG_SOURCES],
}

impl Default for SpatialSourceActivitySnapshot {
    fn default() -> Self {
        Self {
            source_count: 0,
            sources: [SourceActivity::default(); MAX_DEBUG_SOURCES],
        }
    }
}

pub(crate) fn publish_spatial_source_activity(
    source_count: usize,
    sources: [SourceActivity; MAX_DEBUG_SOURCES],
) {
    EPOCH.fetch_add(1, Ordering::AcqRel);
    let source_count = source_count.min(MAX_DEBUG_SOURCES);
    SOURCE_COUNT.store(source_count as u32, Ordering::Relaxed);
    for (index, source) in sources[..source_count].iter().copied().enumerate() {
        let base = index * ACTIVITY_WORDS;
        store_f32(&ACTIVITY[base], source.peak);
        store_f32(&ACTIVITY[base + 1], source.rms);
    }
    for index in source_count..MAX_DEBUG_SOURCES {
        let base = index * ACTIVITY_WORDS;
        store_f32(&ACTIVITY[base], 0.0);
        store_f32(&ACTIVITY[base + 1], 0.0);
    }
    VALID.store(true, Ordering::Release);
    EPOCH.fetch_add(1, Ordering::Release);
}

pub(crate) fn clear_spatial_source_activity() {
    VALID.store(false, Ordering::Release);
}

pub fn spatial_source_activity_latest_snapshot() -> Option<SpatialSourceActivitySnapshot> {
    if !VALID.load(Ordering::Acquire) {
        return None;
    }
    for _ in 0..READ_RETRIES {
        let start = EPOCH.load(Ordering::Acquire);
        if start & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        let source_count = (SOURCE_COUNT.load(Ordering::Relaxed) as usize).min(MAX_DEBUG_SOURCES);
        let mut sources = [SourceActivity::default(); MAX_DEBUG_SOURCES];
        for (index, destination) in sources[..source_count].iter_mut().enumerate() {
            let base = index * ACTIVITY_WORDS;
            destination.peak = load_f32(&ACTIVITY[base]);
            destination.rms = load_f32(&ACTIVITY[base + 1]);
        }
        let end = EPOCH.load(Ordering::Acquire);
        if start == end && end & 1 == 0 && VALID.load(Ordering::Acquire) {
            return Some(SpatialSourceActivitySnapshot {
                source_count,
                sources,
            });
        }
        std::hint::spin_loop();
    }
    None
}

#[inline]
fn store_f32(slot: &AtomicU32, value: f32) {
    let value = if value.is_finite() { value.max(0.0) } else { 0.0 };
    slot.store(value.to_bits(), Ordering::Relaxed);
}

#[inline]
fn load_f32(slot: &AtomicU32) -> f32 {
    f32::from_bits(slot.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_bank_round_trips_fixed_source_slots() {
        let mut activity = [SourceActivity::default(); MAX_DEBUG_SOURCES];
        activity[0] = SourceActivity { peak: 0.9, rms: 0.3 };
        activity[1] = SourceActivity { peak: 0.4, rms: 0.1 };
        publish_spatial_source_activity(2, activity);
        let snapshot = spatial_source_activity_latest_snapshot().expect("activity snapshot");
        assert_eq!(snapshot.source_count, 2);
        assert!((snapshot.sources[0].peak - 0.9).abs() < f32::EPSILON);
        assert!((snapshot.sources[1].rms - 0.1).abs() < f32::EPSILON);
        assert_eq!(snapshot.sources[2], SourceActivity::default());
        clear_spatial_source_activity();
        assert!(spatial_source_activity_latest_snapshot().is_none());
    }
}
