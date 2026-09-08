use std::{f32::consts::FRAC_1_SQRT_2, hint::black_box, time::Instant};

use yinqidao_audio_spatial::{
    ChannelLayout, EngineConfig, SourcePose, SpatialEngine, SpeakerLayout, Trajectory,
    TrajectoryKind, Vec3,
};

const SAMPLE_RATE: u32 = 48_000;
const WARMUP_BLOCKS: usize = 512;
const MEASURED_BLOCKS: usize = 8_192;
const STEREO_HALF_ANGLE_SIN: f32 = 0.5;
const STEREO_HALF_ANGLE_COS: f32 = 0.866_025_4;

#[derive(Clone, Copy)]
enum BenchWorkload {
    Layout(ChannelLayout),
    StereoStatic,
    StereoTrajectory(TrajectoryKind),
}

#[derive(Clone, Copy)]
struct BenchCase {
    name: &'static str,
    workload: BenchWorkload,
    block_frames: usize,
}

const CASES: &[BenchCase] = &[
    BenchCase { name: "stereo/32", workload: BenchWorkload::StereoStatic, block_frames: 32 },
    BenchCase { name: "stereo/64", workload: BenchWorkload::StereoStatic, block_frames: 64 },
    BenchCase { name: "stereo/128", workload: BenchWorkload::StereoStatic, block_frames: 128 },
    BenchCase { name: "orbit360/32", workload: BenchWorkload::StereoTrajectory(TrajectoryKind::Orbit360), block_frames: 32 },
    BenchCase { name: "orbit360/64", workload: BenchWorkload::StereoTrajectory(TrajectoryKind::Orbit360), block_frames: 64 },
    BenchCase { name: "orbit360/128", workload: BenchWorkload::StereoTrajectory(TrajectoryKind::Orbit360), block_frames: 128 },
    BenchCase { name: "figure8/32", workload: BenchWorkload::StereoTrajectory(TrajectoryKind::FigureEight), block_frames: 32 },
    BenchCase { name: "figure8/64", workload: BenchWorkload::StereoTrajectory(TrajectoryKind::FigureEight), block_frames: 64 },
    BenchCase { name: "figure8/128", workload: BenchWorkload::StereoTrajectory(TrajectoryKind::FigureEight), block_frames: 128 },
    BenchCase { name: "5.1.4/32", workload: BenchWorkload::Layout(ChannelLayout::Surround5_1_4), block_frames: 32 },
    BenchCase { name: "5.1.4/64", workload: BenchWorkload::Layout(ChannelLayout::Surround5_1_4), block_frames: 64 },
    BenchCase { name: "5.1.4/128", workload: BenchWorkload::Layout(ChannelLayout::Surround5_1_4), block_frames: 128 },
    BenchCase { name: "7.1.4/32", workload: BenchWorkload::Layout(ChannelLayout::Surround7_1_4), block_frames: 32 },
    BenchCase { name: "7.1.4/64", workload: BenchWorkload::Layout(ChannelLayout::Surround7_1_4), block_frames: 64 },
    BenchCase { name: "7.1.4/128", workload: BenchWorkload::Layout(ChannelLayout::Surround7_1_4), block_frames: 128 },
];

fn main() {
    println!("YinQiDao CPU spatial serial baseline");
    println!("sample_rate={SAMPLE_RATE}Hz warmup={WARMUP_BLOCKS} measured={MEASURED_BLOCKS}");
    println!("Run this example with --release and without other heavy workloads.\n");

    for case in CASES {
        run_case(*case);
    }
}

fn run_case(case: BenchCase) {
    let channels = workload_channels(case.workload);
    let mut config = EngineConfig::new(SAMPLE_RATE);
    config.block_frames = case.block_frames;
    config.environment.mix = 0.0;
    let mut engine = SpatialEngine::new(config).expect("valid benchmark engine config");
    let mut trajectory = trajectory_for(case.workload);

    let mut input = vec![0.0_f32; case.block_frames * channels];
    fill_deterministic_pcm(&mut input, channels);
    let mut output = vec![0.0_f32; case.block_frames * 2];

    for _ in 0..WARMUP_BLOCKS {
        render_block(
            &mut engine,
            case,
            black_box(&input),
            black_box(&mut output),
            &mut trajectory,
        );
    }
    engine.reset();
    if let Some(trajectory) = trajectory.as_mut() {
        trajectory.reset();
    }

    let mut samples_ns = Vec::with_capacity(MEASURED_BLOCKS);
    for _ in 0..MEASURED_BLOCKS {
        let start = Instant::now();
        render_block(
            &mut engine,
            case,
            black_box(&input),
            black_box(&mut output),
            &mut trajectory,
        );
        samples_ns.push(start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        black_box(output[0]);
    }

    samples_ns.sort_unstable();
    let sum = samples_ns.iter().copied().map(u128::from).sum::<u128>();
    let average_ns = sum as f64 / samples_ns.len() as f64;
    let p50 = percentile(&samples_ns, 0.50);
    let p95 = percentile(&samples_ns, 0.95);
    let p99 = percentile(&samples_ns, 0.99);
    let worst = samples_ns.last().copied().unwrap_or(0);
    let deadline_ns = case.block_frames as f64 * 1_000_000_000.0 / f64::from(SAMPLE_RATE);
    let average_budget_pct = average_ns / deadline_ns * 100.0;
    let p99_budget_pct = p99 as f64 / deadline_ns * 100.0;

    println!(
        "{:<13} backend={:?} sources={:<2} block={:<3} avg={:>8.1}ns p50={:>7}ns p95={:>7}ns p99={:>7}ns worst={:>8}ns budget(avg/p99)={:>5.2}%/{:>5.2}%",
        case.name,
        engine.simd_backend(),
        channels,
        case.block_frames,
        average_ns,
        p50,
        p95,
        p99,
        worst,
        average_budget_pct,
        p99_budget_pct,
    );
}

fn render_block(
    engine: &mut SpatialEngine,
    case: BenchCase,
    input: &[f32],
    output: &mut [f32],
    trajectory: &mut Option<Trajectory>,
) {
    match case.workload {
        BenchWorkload::Layout(layout) => {
            engine
                .render_interleaved_layout(input, layout, output)
                .expect("benchmark layout render");
        }
        BenchWorkload::StereoStatic => {
            let center = SourcePose::new(Vec3::new(0.0, 0.0, 1.0));
            let (left, right) = stereo_pair(center);
            engine
                .render_interleaved_stereo_pair(input, left, left, right, right, output)
                .expect("benchmark stereo render");
        }
        BenchWorkload::StereoTrajectory(_) => {
            let trajectory = trajectory.as_mut().expect("trajectory workload");
            let (center_start, center_end) = trajectory.next_segment(case.block_frames);
            let (left_start, right_start) = stereo_pair(center_start);
            let (left_end, right_end) = stereo_pair(center_end);
            engine
                .render_interleaved_stereo_pair(
                    input,
                    left_start,
                    left_end,
                    right_start,
                    right_end,
                    output,
                )
                .expect("benchmark trajectory render");
        }
    }
}

fn workload_channels(workload: BenchWorkload) -> usize {
    match workload {
        BenchWorkload::Layout(layout) => SpeakerLayout::for_layout(layout).channels(),
        BenchWorkload::StereoStatic | BenchWorkload::StereoTrajectory(_) => 2,
    }
}

fn trajectory_for(workload: BenchWorkload) -> Option<Trajectory> {
    match workload {
        BenchWorkload::StereoTrajectory(kind) => {
            Some(Trajectory::new(kind, SAMPLE_RATE, 0.10, 1.0, 0.0))
        }
        _ => None,
    }
}

fn stereo_pair(center: SourcePose) -> (SourcePose, SourcePose) {
    (
        SourcePose {
            position: rotate_y(center.position, -STEREO_HALF_ANGLE_SIN, STEREO_HALF_ANGLE_COS),
            velocity: center.velocity,
            gain: FRAC_1_SQRT_2,
            spread: 0.1,
        },
        SourcePose {
            position: rotate_y(center.position, STEREO_HALF_ANGLE_SIN, STEREO_HALF_ANGLE_COS),
            velocity: center.velocity,
            gain: FRAC_1_SQRT_2,
            spread: 0.1,
        },
    )
}

fn rotate_y(position: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(
        position.x * cos + position.z * sin,
        position.y,
        -position.x * sin + position.z * cos,
    )
}

fn percentile(sorted: &[u64], percentile: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() - 1) as f64 * percentile)
        .round()
        .clamp(0.0, (sorted.len() - 1) as f64) as usize;
    sorted[index]
}

fn fill_deterministic_pcm(samples: &mut [f32], channels: usize) {
    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    for (index, sample) in samples.iter_mut().enumerate() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let noise = ((state >> 40) as i32 - 0x7f_ffff) as f32 / 0x7f_ffff as f32;
        let channel_gain = 0.15 + (index % channels) as f32 * 0.01;
        *sample = noise * channel_gain;
    }
}
