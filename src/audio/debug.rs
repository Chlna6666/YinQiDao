use std::{
    collections::VecDeque,
    f32::consts::PI,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

const SPECTRUM_BINS: usize = 48;
const WAVEFORM_POINTS: usize = 384;
const PHASE_HISTORY_POINTS: usize = 180;
const SPECTROGRAM_ROWS: usize = 72;
const ANALYSIS_INTERVAL: Duration = Duration::from_millis(30);
const DB_FLOOR: f32 = -120.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum AudioDebugMonitorMode {
    Source = 0,
    PostEq = 1,
    PostSpatial = 2,
}

impl AudioDebugMonitorMode {
    fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::Source,
            1 => Self::PostEq,
            _ => Self::PostSpatial,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "A · SOURCE",
            Self::PostEq => "B · POST-EQ",
            Self::PostSpatial => "C · POST-SPATIAL",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AudioDebugStage {
    pub peak_dbfs: f32,
    pub true_peak_dbtp: f32,
    pub rms_dbfs: f32,
    pub crest_db: f32,
    pub stereo_correlation: f32,
    pub side_mid_db: f32,
    pub clipped_samples: u64,
    pub lufs_momentary: f32,
    pub lufs_short_term: f32,
    pub lufs_integrated: f32,
    pub dynamic_range_db: f32,
    pub spectrum_dbfs: Vec<f32>,
    pub mid_spectrum_dbfs: Vec<f32>,
    pub side_spectrum_dbfs: Vec<f32>,
    pub waveform_left: Vec<f32>,
    pub waveform_right: Vec<f32>,
}

impl Default for AudioDebugStage {
    fn default() -> Self {
        Self {
            peak_dbfs: DB_FLOOR,
            true_peak_dbtp: DB_FLOOR,
            rms_dbfs: DB_FLOOR,
            crest_db: 0.0,
            stereo_correlation: 0.0,
            side_mid_db: DB_FLOOR,
            clipped_samples: 0,
            lufs_momentary: DB_FLOOR,
            lufs_short_term: DB_FLOOR,
            lufs_integrated: DB_FLOOR,
            dynamic_range_db: 0.0,
            spectrum_dbfs: vec![DB_FLOOR; SPECTRUM_BINS],
            mid_spectrum_dbfs: vec![DB_FLOOR; SPECTRUM_BINS],
            side_spectrum_dbfs: vec![DB_FLOOR; SPECTRUM_BINS],
            waveform_left: vec![0.0; WAVEFORM_POINTS],
            waveform_right: vec![0.0; WAVEFORM_POINTS],
        }
    }
}

#[derive(Clone, Debug)]
pub struct AudioDebugSnapshot {
    pub sequence: u64,
    pub sample_rate: u32,
    pub monitor_mode: AudioDebugMonitorMode,
    pub source: AudioDebugStage,
    pub eq: AudioDebugStage,
    pub spatial: AudioDebugStage,
    pub eq_transfer_db: Vec<f32>,
    pub spatial_transfer_db: Vec<f32>,
    pub phase_history: Vec<f32>,
    pub crest_history: Vec<f32>,
    pub spectrogram: Vec<Vec<f32>>,
}

impl Default for AudioDebugSnapshot {
    fn default() -> Self {
        Self {
            sequence: 0,
            sample_rate: 0,
            monitor_mode: AudioDebugMonitorMode::PostSpatial,
            source: AudioDebugStage::default(),
            eq: AudioDebugStage::default(),
            spatial: AudioDebugStage::default(),
            eq_transfer_db: vec![0.0; SPECTRUM_BINS],
            spatial_transfer_db: vec![0.0; SPECTRUM_BINS],
            phase_history: Vec::new(),
            crest_history: Vec::new(),
            spectrogram: Vec::new(),
        }
    }
}

struct StageHistory {
    momentary_energy: VecDeque<f64>,
    short_energy: VecDeque<f64>,
    integrated_blocks: Vec<f64>,
    rms_history_db: VecDeque<f32>,
}

impl StageHistory {
    fn new() -> Self {
        Self {
            momentary_energy: VecDeque::new(),
            short_energy: VecDeque::new(),
            integrated_blocks: Vec::new(),
            rms_history_db: VecDeque::new(),
        }
    }

    fn update(&mut self, energy: f64, rms_db: f32) -> (f32, f32, f32, f32) {
        self.momentary_energy.push_back(energy);
        while self.momentary_energy.len() > 14 {
            self.momentary_energy.pop_front();
        }

        self.short_energy.push_back(energy);
        while self.short_energy.len() > 100 {
            self.short_energy.pop_front();
        }

        let momentary = loudness_from_energy(mean_energy(&self.momentary_energy));
        let short = loudness_from_energy(mean_energy(&self.short_energy));

        if momentary.is_finite() && momentary > -70.0 {
            self.integrated_blocks.push(energy);
            if self.integrated_blocks.len() > 120_000 {
                self.integrated_blocks.drain(..60_000);
            }
        }

        let absolute = self
            .integrated_blocks
            .iter()
            .copied()
            .filter(|energy| loudness_from_energy(*energy) > -70.0)
            .collect::<Vec<_>>();
        let preliminary = loudness_from_energy(mean_slice(&absolute));
        let relative_gate = preliminary - 10.0;
        let gated = absolute
            .iter()
            .copied()
            .filter(|energy| loudness_from_energy(*energy) >= relative_gate)
            .collect::<Vec<_>>();
        let integrated = loudness_from_energy(mean_slice(&gated));

        self.rms_history_db.push_back(rms_db);
        while self.rms_history_db.len() > 300 {
            self.rms_history_db.pop_front();
        }
        let dynamic_range = if self.rms_history_db.len() >= 8 {
            percentile_range(&self.rms_history_db, 0.10, 0.95)
        } else {
            0.0
        };

        (momentary, short, integrated, dynamic_range)
    }
}

struct AnalyzerState {
    last_capture: Instant,
    snapshot: AudioDebugSnapshot,
    source_history: StageHistory,
    eq_history: StageHistory,
    spatial_history: StageHistory,
    phase_history: VecDeque<f32>,
    crest_history: VecDeque<f32>,
    spectrogram: VecDeque<Vec<f32>>,
}

impl AnalyzerState {
    fn new() -> Self {
        Self {
            last_capture: Instant::now() - ANALYSIS_INTERVAL,
            snapshot: AudioDebugSnapshot::default(),
            source_history: StageHistory::new(),
            eq_history: StageHistory::new(),
            spatial_history: StageHistory::new(),
            phase_history: VecDeque::new(),
            crest_history: VecDeque::new(),
            spectrogram: VecDeque::new(),
        }
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static MONITOR_MODE: AtomicU8 = AtomicU8::new(AudioDebugMonitorMode::PostSpatial as u8);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static STATE: OnceLock<Mutex<AnalyzerState>> = OnceLock::new();

fn state() -> &'static Mutex<AnalyzerState> {
    STATE.get_or_init(|| Mutex::new(AnalyzerState::new()))
}

pub fn set_audio_debug_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
}

#[inline]
pub fn audio_debug_enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

pub fn set_audio_debug_monitor_mode(mode: AudioDebugMonitorMode) {
    MONITOR_MODE.store(mode as u8, Ordering::Release);
}

#[inline]
pub fn audio_debug_monitor_mode() -> AudioDebugMonitorMode {
    AudioDebugMonitorMode::from_raw(MONITOR_MODE.load(Ordering::Acquire))
}

pub fn audio_debug_latest_snapshot() -> AudioDebugSnapshot {
    state()
        .lock()
        .map(|state| state.snapshot.clone())
        .unwrap_or_default()
}

pub(crate) fn capture_audio_debug_frame(
    source: &[f32],
    post_eq: &[f32],
    post_spatial: &[f32],
    sample_rate: u32,
) {
    if !audio_debug_enabled() || source.len() < 4 || post_eq.len() < 4 || post_spatial.len() < 4 {
        return;
    }

    let Ok(mut state) = state().try_lock() else {
        return;
    };
    if state.last_capture.elapsed() < ANALYSIS_INTERVAL {
        return;
    }
    state.last_capture = Instant::now();

    let source_stage = analyze_stage(source, sample_rate, &mut state.source_history);
    let eq_stage = analyze_stage(post_eq, sample_rate, &mut state.eq_history);
    let spatial_stage = analyze_stage(post_spatial, sample_rate, &mut state.spatial_history);

    state
        .phase_history
        .push_back(spatial_stage.stereo_correlation.clamp(-1.0, 1.0));
    while state.phase_history.len() > PHASE_HISTORY_POINTS {
        state.phase_history.pop_front();
    }

    state.crest_history.push_back(spatial_stage.crest_db.clamp(0.0, 48.0));
    while state.crest_history.len() > PHASE_HISTORY_POINTS {
        state.crest_history.pop_front();
    }

    state
        .spectrogram
        .push_back(spatial_stage.spectrum_dbfs.clone());
    while state.spectrogram.len() > SPECTROGRAM_ROWS {
        state.spectrogram.pop_front();
    }

    let eq_transfer_db = transfer_curve(&source_stage.spectrum_dbfs, &eq_stage.spectrum_dbfs);
    let spatial_transfer_db =
        transfer_curve(&eq_stage.spectrum_dbfs, &spatial_stage.spectrum_dbfs);

    state.snapshot = AudioDebugSnapshot {
        sequence: SEQUENCE.fetch_add(1, Ordering::AcqRel) + 1,
        sample_rate,
        monitor_mode: audio_debug_monitor_mode(),
        source: source_stage,
        eq: eq_stage,
        spatial: spatial_stage,
        eq_transfer_db,
        spatial_transfer_db,
        phase_history: state.phase_history.iter().copied().collect(),
        crest_history: state.crest_history.iter().copied().collect(),
        spectrogram: state.spectrogram.iter().cloned().collect(),
    };
}

fn analyze_stage(samples: &[f32], sample_rate: u32, history: &mut StageHistory) -> AudioDebugStage {
    let frames = samples.len() / 2;
    if frames == 0 {
        return AudioDebugStage::default();
    }

    let mut peak = 0.0_f32;
    let mut clipped_samples = 0_u64;
    let mut sum_square = 0.0_f64;
    let mut sum_lr = 0.0_f64;
    let mut sum_l2 = 0.0_f64;
    let mut sum_r2 = 0.0_f64;
    let mut mid_energy = 0.0_f64;
    let mut side_energy = 0.0_f64;

    for frame in samples.chunks_exact(2) {
        let left = frame[0];
        let right = frame[1];
        peak = peak.max(left.abs()).max(right.abs());
        clipped_samples += if left.abs() >= 1.0 { 1 } else { 0 };
        clipped_samples += if right.abs() >= 1.0 { 1 } else { 0 };

        let l = f64::from(left);
        let r = f64::from(right);
        sum_square += l * l + r * r;
        sum_lr += l * r;
        sum_l2 += l * l;
        sum_r2 += r * r;
        let mid = (l + r) * 0.5;
        let side = (l - r) * 0.5;
        mid_energy += mid * mid;
        side_energy += side * side;
    }

    let rms = (sum_square / (frames as f64 * 2.0)).sqrt() as f32;
    let peak_dbfs = amp_to_db(peak);
    let rms_dbfs = amp_to_db(rms);
    let crest_db = (peak_dbfs - rms_dbfs).max(0.0);
    let stereo_correlation = if sum_l2 > 1.0e-18 && sum_r2 > 1.0e-18 {
        (sum_lr / (sum_l2.sqrt() * sum_r2.sqrt())).clamp(-1.0, 1.0) as f32
    } else {
        0.0
    };
    let side_mid_db = if mid_energy > 1.0e-18 {
        power_to_db(side_energy / mid_energy)
    } else {
        DB_FLOOR
    };

    let true_peak = true_peak_4x(samples);
    let weighted_energy = approximate_k_weighted_energy(samples, sample_rate);
    let (lufs_momentary, lufs_short_term, lufs_integrated, dynamic_range_db) =
        history.update(weighted_energy, rms_dbfs);

    let (waveform_left, waveform_right) = downsample_waveform(samples);
    let spectrum_dbfs = spectrum(samples, sample_rate, SpectrumMode::Stereo);
    let mid_spectrum_dbfs = spectrum(samples, sample_rate, SpectrumMode::Mid);
    let side_spectrum_dbfs = spectrum(samples, sample_rate, SpectrumMode::Side);

    AudioDebugStage {
        peak_dbfs,
        true_peak_dbtp: amp_to_db(true_peak),
        rms_dbfs,
        crest_db,
        stereo_correlation,
        side_mid_db,
        clipped_samples,
        lufs_momentary,
        lufs_short_term,
        lufs_integrated,
        dynamic_range_db,
        spectrum_dbfs,
        mid_spectrum_dbfs,
        side_spectrum_dbfs,
        waveform_left,
        waveform_right,
    }
}

fn transfer_curve(before: &[f32], after: &[f32]) -> Vec<f32> {
    before
        .iter()
        .copied()
        .zip(after.iter().copied())
        .map(|(before, after)| (after - before).clamp(-36.0, 36.0))
        .collect()
}

fn downsample_waveform(samples: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let frames = samples.len() / 2;
    let points = frames.min(WAVEFORM_POINTS).max(1);
    let stride = (frames / points).max(1);
    let mut left = Vec::with_capacity(points);
    let mut right = Vec::with_capacity(points);
    let mut frame = 0;
    while frame < frames && left.len() < points {
        left.push(samples[frame * 2]);
        right.push(samples[frame * 2 + 1]);
        frame = frame.saturating_add(stride);
    }
    (left, right)
}

#[derive(Clone, Copy)]
enum SpectrumMode {
    Stereo,
    Mid,
    Side,
}

fn spectrum(samples: &[f32], sample_rate: u32, mode: SpectrumMode) -> Vec<f32> {
    let frames = (samples.len() / 2).min(768);
    if frames < 8 || sample_rate == 0 {
        return vec![DB_FLOOR; SPECTRUM_BINS];
    }

    let start_frame = samples.len() / 2 - frames;
    let nyquist = sample_rate as f32 * 0.5;
    let max_frequency = nyquist.min(20_000.0).max(80.0);
    let min_frequency = 20.0_f32.min(max_frequency * 0.5).max(10.0);
    let log_span = (max_frequency / min_frequency).ln();

    let mut result = Vec::with_capacity(SPECTRUM_BINS);
    for bin in 0..SPECTRUM_BINS {
        let t = if SPECTRUM_BINS <= 1 {
            0.0
        } else {
            bin as f32 / (SPECTRUM_BINS - 1) as f32
        };
        let frequency = min_frequency * (log_span * t).exp();
        let omega = 2.0 * PI * frequency / sample_rate as f32;
        let coefficient = 2.0 * omega.cos();
        let mut q1 = 0.0_f32;
        let mut q2 = 0.0_f32;

        for index in 0..frames {
            let source_index = (start_frame + index) * 2;
            let left = samples[source_index];
            let right = samples[source_index + 1];
            let sample = match mode {
                SpectrumMode::Stereo => (left + right) * 0.5,
                SpectrumMode::Mid => (left + right) * 0.5,
                SpectrumMode::Side => (left - right) * 0.5,
            };
            let window =
                0.5 - 0.5 * (2.0 * PI * index as f32 / (frames - 1) as f32).cos();
            let q0 = sample * window + coefficient * q1 - q2;
            q2 = q1;
            q1 = q0;
        }

        let power = (q1 * q1 + q2 * q2 - coefficient * q1 * q2).max(1.0e-20);
        let magnitude = power.sqrt() / (frames as f32 * 0.5);
        result.push(amp_to_db(magnitude));
    }
    result
}

fn true_peak_4x(samples: &[f32]) -> f32 {
    let mut peak = 0.0_f32;
    for channel in 0..2 {
        let mut previous = samples.get(channel).copied().unwrap_or(0.0);
        peak = peak.max(previous.abs());
        for frame in samples.chunks_exact(2).skip(1) {
            let current = frame[channel];
            for phase in 1..=4 {
                let t = phase as f32 * 0.25;
                let interpolated = previous + (current - previous) * t;
                peak = peak.max(interpolated.abs());
            }
            previous = current;
        }
    }
    peak
}

fn approximate_k_weighted_energy(samples: &[f32], sample_rate: u32) -> f64 {
    if sample_rate == 0 {
        return 0.0;
    }

    // BS.1770-style debug meter: apply a lightweight high-pass and high-shelf approximation.
    // It is intentionally kept off the playback path and is only active while Audio Laboratory is open.
    let mut left_hp = 0.0_f32;
    let mut right_hp = 0.0_f32;
    let mut previous_left = 0.0_f32;
    let mut previous_right = 0.0_f32;
    let hp_rc = 1.0 / (2.0 * PI * 60.0);
    let dt = 1.0 / sample_rate as f32;
    let hp_alpha = hp_rc / (hp_rc + dt);
    let shelf_gain = 10.0_f32.powf(4.0 / 20.0);
    let mut energy = 0.0_f64;
    let mut frames = 0_u64;

    for frame in samples.chunks_exact(2) {
        let left = hp_alpha * (left_hp + frame[0] - previous_left);
        let right = hp_alpha * (right_hp + frame[1] - previous_right);
        left_hp = left;
        right_hp = right;
        previous_left = frame[0];
        previous_right = frame[1];

        let weighted_left = left * shelf_gain;
        let weighted_right = right * shelf_gain;
        energy += f64::from(weighted_left * weighted_left + weighted_right * weighted_right);
        frames += 1;
    }

    if frames == 0 {
        0.0
    } else {
        energy / frames as f64
    }
}

fn loudness_from_energy(energy: f64) -> f32 {
    if energy <= 1.0e-20 {
        DB_FLOOR
    } else {
        (-0.691 + 10.0 * energy.log10()) as f32
    }
}

fn mean_energy(values: &VecDeque<f64>) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn mean_slice(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn percentile_range(values: &VecDeque<f32>, low: f32, high: f32) -> f32 {
    let mut sorted = values.iter().copied().filter(|v| v.is_finite()).collect::<Vec<_>>();
    if sorted.len() < 2 {
        return 0.0;
    }
    sorted.sort_by(f32::total_cmp);
    let low_index = ((sorted.len() - 1) as f32 * low.clamp(0.0, 1.0)).round() as usize;
    let high_index = ((sorted.len() - 1) as f32 * high.clamp(0.0, 1.0)).round() as usize;
    (sorted[high_index] - sorted[low_index]).max(0.0)
}

#[inline]
fn amp_to_db(value: f32) -> f32 {
    if value <= 1.0e-12 {
        DB_FLOOR
    } else {
        (20.0 * value.log10()).max(DB_FLOOR)
    }
}

#[inline]
fn power_to_db(value: f64) -> f32 {
    if value <= 1.0e-12 {
        DB_FLOOR
    } else {
        (10.0 * value.log10()) as f32
    }
}
