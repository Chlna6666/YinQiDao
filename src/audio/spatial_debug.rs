use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use yinqidao_audio_spatial::{
    EnvironmentSettings, ListenerPose, MAX_DEBUG_REFLECTIONS, MAX_DEBUG_SOURCES,
    SpatialDebugReflection, SpatialDebugReflectionWall, SpatialDebugSnapshot, SpatialDebugSource,
    SpatialDebugSourceKind, Vec3,
};

const SOURCE_WORDS: usize = 24;
const REFLECTION_WORDS: usize = 12;
const LISTENER_WORDS: usize = 9;
const ENVIRONMENT_WORDS: usize = 4;
const READ_RETRIES: usize = 4;

static VALID: AtomicBool = AtomicBool::new(false);
static EPOCH: AtomicU64 = AtomicU64::new(0);
static SNAPSHOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SAMPLE_RATE: AtomicU32 = AtomicU32::new(0);
static RENDERED_FRAMES: AtomicU64 = AtomicU64::new(0);
static SOURCE_COUNT: AtomicU32 = AtomicU32::new(0);
static REFLECTION_COUNT: AtomicU32 = AtomicU32::new(0);
static LISTENER: [AtomicU32; LISTENER_WORDS] = [const { AtomicU32::new(0) }; LISTENER_WORDS];
static ENVIRONMENT: [AtomicU32; ENVIRONMENT_WORDS] =
    [const { AtomicU32::new(0) }; ENVIRONMENT_WORDS];
static SOURCES: [AtomicU32; MAX_DEBUG_SOURCES * SOURCE_WORDS] =
    [const { AtomicU32::new(0) }; MAX_DEBUG_SOURCES * SOURCE_WORDS];
static REFLECTIONS: [AtomicU32; MAX_DEBUG_REFLECTIONS * REFLECTION_WORDS] =
    [const { AtomicU32::new(0) }; MAX_DEBUG_REFLECTIONS * REFLECTION_WORDS];

/// Publish a spatial scene without allocation, locks or blocking. The surrounding odd/even epoch
/// gives readers a consistent multi-atomic snapshot; all payload slots remain ordinary atomics so a
/// concurrent reader/writer can never create a Rust data race.
pub(crate) fn publish_spatial_debug_snapshot(snapshot: SpatialDebugSnapshot) {
    EPOCH.fetch_add(1, Ordering::AcqRel);

    SNAPSHOT_SEQUENCE.store(snapshot.sequence, Ordering::Relaxed);
    SAMPLE_RATE.store(snapshot.sample_rate, Ordering::Relaxed);
    RENDERED_FRAMES.store(snapshot.rendered_frames, Ordering::Relaxed);
    SOURCE_COUNT.store(
        snapshot.source_count.min(MAX_DEBUG_SOURCES) as u32,
        Ordering::Relaxed,
    );
    REFLECTION_COUNT.store(
        snapshot.reflection_count.min(MAX_DEBUG_REFLECTIONS) as u32,
        Ordering::Relaxed,
    );

    store_f32(&LISTENER[0], snapshot.listener.position.x);
    store_f32(&LISTENER[1], snapshot.listener.position.y);
    store_f32(&LISTENER[2], snapshot.listener.position.z);
    store_f32(&LISTENER[3], snapshot.listener.forward.x);
    store_f32(&LISTENER[4], snapshot.listener.forward.y);
    store_f32(&LISTENER[5], snapshot.listener.forward.z);
    store_f32(&LISTENER[6], snapshot.listener.up.x);
    store_f32(&LISTENER[7], snapshot.listener.up.y);
    store_f32(&LISTENER[8], snapshot.listener.up.z);

    store_f32(&ENVIRONMENT[0], snapshot.environment.mix);
    store_f32(&ENVIRONMENT[1], snapshot.environment.room_size);
    store_f32(&ENVIRONMENT[2], snapshot.environment.damping);
    store_f32(&ENVIRONMENT[3], snapshot.environment_contribution);

    for (index, source) in snapshot.sources[..snapshot.source_count.min(MAX_DEBUG_SOURCES)]
        .iter()
        .copied()
        .enumerate()
    {
        store_source(index, source);
    }
    for (index, reflection) in snapshot.reflections
        [..snapshot.reflection_count.min(MAX_DEBUG_REFLECTIONS)]
        .iter()
        .copied()
        .enumerate()
    {
        store_reflection(index, reflection);
    }

    VALID.store(true, Ordering::Release);
    EPOCH.fetch_add(1, Ordering::Release);
}

pub(crate) fn clear_spatial_debug_snapshot() {
    VALID.store(false, Ordering::Release);
}

/// Read the latest complete spatial scene. The copy is fixed-size and contains no heap storage.
pub fn spatial_debug_latest_snapshot() -> Option<SpatialDebugSnapshot> {
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
        let reflection_count =
            (REFLECTION_COUNT.load(Ordering::Relaxed) as usize).min(MAX_DEBUG_REFLECTIONS);
        let mut sources = [SpatialDebugSource::default(); MAX_DEBUG_SOURCES];
        for (index, destination) in sources[..source_count].iter_mut().enumerate() {
            *destination = load_source(index);
        }
        let mut reflections = [SpatialDebugReflection::default(); MAX_DEBUG_REFLECTIONS];
        for (index, destination) in reflections[..reflection_count].iter_mut().enumerate() {
            *destination = load_reflection(index);
        }

        let snapshot = SpatialDebugSnapshot {
            sequence: SNAPSHOT_SEQUENCE.load(Ordering::Relaxed),
            sample_rate: SAMPLE_RATE.load(Ordering::Relaxed),
            rendered_frames: RENDERED_FRAMES.load(Ordering::Relaxed),
            listener: ListenerPose {
                position: Vec3::new(
                    load_f32(&LISTENER[0]),
                    load_f32(&LISTENER[1]),
                    load_f32(&LISTENER[2]),
                ),
                forward: Vec3::new(
                    load_f32(&LISTENER[3]),
                    load_f32(&LISTENER[4]),
                    load_f32(&LISTENER[5]),
                ),
                up: Vec3::new(
                    load_f32(&LISTENER[6]),
                    load_f32(&LISTENER[7]),
                    load_f32(&LISTENER[8]),
                ),
            },
            environment: EnvironmentSettings {
                mix: load_f32(&ENVIRONMENT[0]),
                room_size: load_f32(&ENVIRONMENT[1]),
                damping: load_f32(&ENVIRONMENT[2]),
            },
            environment_contribution: load_f32(&ENVIRONMENT[3]),
            source_count,
            sources,
            reflection_count,
            reflections,
        };

        let end = EPOCH.load(Ordering::Acquire);
        if start == end && end & 1 == 0 && VALID.load(Ordering::Acquire) {
            return Some(snapshot);
        }
        std::hint::spin_loop();
    }
    None
}

fn store_source(index: usize, source: SpatialDebugSource) {
    let base = index * SOURCE_WORDS;
    SOURCES[base].store(if source.active { 1 } else { 0 }, Ordering::Relaxed);
    SOURCES[base + 1].store(u32::from(source.source_index), Ordering::Relaxed);
    SOURCES[base + 2].store(
        match source.kind {
            SpatialDebugSourceKind::FullRange => 0,
            SpatialDebugSourceKind::Lfe => 1,
        },
        Ordering::Relaxed,
    );
    for (slot, value) in [
        source.position.x,
        source.position.y,
        source.position.z,
        source.velocity.x,
        source.velocity.y,
        source.velocity.z,
        source.gain,
        source.spread,
        source.azimuth_degrees,
        source.elevation_degrees,
        source.distance_meters,
        source.left_delay_samples,
        source.right_delay_samples,
        source.itd_samples,
        source.ild_db,
        source.left_gain,
        source.right_gain,
        source.near_field_amount,
        source.head_shadow_amount,
        source.air_absorption_amount,
        source.direct_contribution,
    ]
    .into_iter()
    .enumerate()
    {
        store_f32(&SOURCES[base + 3 + slot], value);
    }
}

fn load_source(index: usize) -> SpatialDebugSource {
    let base = index * SOURCE_WORDS;
    let value = |offset: usize| load_f32(&SOURCES[base + offset]);
    SpatialDebugSource {
        active: SOURCES[base].load(Ordering::Relaxed) != 0,
        source_index: SOURCES[base + 1].load(Ordering::Relaxed).min(u32::from(u16::MAX)) as u16,
        kind: if SOURCES[base + 2].load(Ordering::Relaxed) == 1 {
            SpatialDebugSourceKind::Lfe
        } else {
            SpatialDebugSourceKind::FullRange
        },
        position: Vec3::new(value(3), value(4), value(5)),
        velocity: Vec3::new(value(6), value(7), value(8)),
        gain: value(9),
        spread: value(10),
        azimuth_degrees: value(11),
        elevation_degrees: value(12),
        distance_meters: value(13),
        left_delay_samples: value(14),
        right_delay_samples: value(15),
        itd_samples: value(16),
        ild_db: value(17),
        left_gain: value(18),
        right_gain: value(19),
        near_field_amount: value(20),
        head_shadow_amount: value(21),
        air_absorption_amount: value(22),
        direct_contribution: value(23),
    }
}

fn store_reflection(index: usize, reflection: SpatialDebugReflection) {
    let base = index * REFLECTION_WORDS;
    REFLECTIONS[base].store(if reflection.active { 1 } else { 0 }, Ordering::Relaxed);
    REFLECTIONS[base + 1].store(u32::from(reflection.tap_index), Ordering::Relaxed);
    REFLECTIONS[base + 2].store(
        match reflection.wall {
            SpatialDebugReflectionWall::Left => 0,
            SpatialDebugReflectionWall::Right => 1,
            SpatialDebugReflectionWall::Front => 2,
            SpatialDebugReflectionWall::Rear => 3,
        },
        Ordering::Relaxed,
    );
    store_f32(&REFLECTIONS[base + 3], reflection.virtual_position.x);
    store_f32(&REFLECTIONS[base + 4], reflection.virtual_position.y);
    store_f32(&REFLECTIONS[base + 5], reflection.virtual_position.z);
    REFLECTIONS[base + 6].store(reflection.delay_samples, Ordering::Relaxed);
    store_f32(&REFLECTIONS[base + 7], reflection.delay_milliseconds);
    store_f32(&REFLECTIONS[base + 8], reflection.path_length_meters);
    store_f32(&REFLECTIONS[base + 9], reflection.gain);
    store_f32(&REFLECTIONS[base + 10], reflection.wet_contribution);
    REFLECTIONS[base + 11].store(if reflection.cross_ear { 1 } else { 0 }, Ordering::Relaxed);
}

fn load_reflection(index: usize) -> SpatialDebugReflection {
    let base = index * REFLECTION_WORDS;
    SpatialDebugReflection {
        active: REFLECTIONS[base].load(Ordering::Relaxed) != 0,
        tap_index: REFLECTIONS[base + 1]
            .load(Ordering::Relaxed)
            .min(u32::from(u8::MAX)) as u8,
        wall: match REFLECTIONS[base + 2].load(Ordering::Relaxed) {
            0 => SpatialDebugReflectionWall::Left,
            1 => SpatialDebugReflectionWall::Right,
            3 => SpatialDebugReflectionWall::Rear,
            _ => SpatialDebugReflectionWall::Front,
        },
        virtual_position: Vec3::new(
            load_f32(&REFLECTIONS[base + 3]),
            load_f32(&REFLECTIONS[base + 4]),
            load_f32(&REFLECTIONS[base + 5]),
        ),
        delay_samples: REFLECTIONS[base + 6].load(Ordering::Relaxed),
        delay_milliseconds: load_f32(&REFLECTIONS[base + 7]),
        path_length_meters: load_f32(&REFLECTIONS[base + 8]),
        gain: load_f32(&REFLECTIONS[base + 9]),
        wet_contribution: load_f32(&REFLECTIONS[base + 10]),
        cross_ear: REFLECTIONS[base + 11].load(Ordering::Relaxed) != 0,
    }
}

#[inline]
fn store_f32(slot: &AtomicU32, value: f32) {
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
    fn atomic_publication_round_trips_fixed_scene() {
        let mut source = SpatialDebugSource::default();
        source.active = true;
        source.source_index = 0;
        source.kind = SpatialDebugSourceKind::FullRange;
        source.position = Vec3::RIGHT;
        source.azimuth_degrees = 90.0;
        source.ild_db = 3.0;
        let mut snapshot = SpatialDebugSnapshot::new(48_000);
        snapshot.sequence = 7;
        snapshot.rendered_frames = 1_600;
        snapshot.source_count = 1;
        snapshot.sources[0] = source;
        snapshot.reflection_count = 1;
        snapshot.reflections[0] = SpatialDebugReflection {
            active: true,
            tap_index: 0,
            wall: SpatialDebugReflectionWall::Left,
            virtual_position: Vec3::new(-1.5, 0.0, 0.0),
            delay_samples: 240,
            delay_milliseconds: 5.0,
            path_length_meters: 1.715,
            gain: 0.30,
            wet_contribution: 0.03,
            cross_ear: true,
        };
        publish_spatial_debug_snapshot(snapshot);

        let read = spatial_debug_latest_snapshot().expect("published snapshot");
        assert_eq!(read.sequence, 7);
        assert_eq!(read.rendered_frames, 1_600);
        assert_eq!(read.source_count, 1);
        assert_eq!(read.sources[0].position, Vec3::RIGHT);
        assert!((read.sources[0].ild_db - 3.0).abs() < f32::EPSILON);
        assert_eq!(read.reflection_count, 1);
        assert_eq!(read.reflections[0].wall, SpatialDebugReflectionWall::Left);
        assert_eq!(read.reflections[0].delay_samples, 240);

        clear_spatial_debug_snapshot();
        assert!(spatial_debug_latest_snapshot().is_none());
    }
}
