use std::fs;

use crossbeam_channel::unbounded;

use super::*;

#[allow(clippy::type_complexity)]
fn test_worker(
    ring_capacity: usize,
) -> (
    AudioWorker,
    ringbuf::HeapCons<f32>,
    Arc<CommandQueue<PlayerCommand>>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
) {
    let command_queue = Arc::new(CommandQueue::new());
    let (event_tx, _event_rx) = unbounded();
    let ring = HeapRb::<f32>::new(ring_capacity);
    let (producer, consumer) = ring.split();
    let flush = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let worker = AudioWorker::new(
        command_queue.clone(),
        event_tx,
        producer,
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(PlayerSnapshot::default())),
        flush.clone(),
        paused.clone(),
        AudioWorkerConfig {
            output_rate: 8_000,
            eq: EqSettings::default(),
            spatial: SpatialSettings::default(),
        },
    );
    (worker, consumer, command_queue, flush, paused)
}

fn test_track(id: TrackId, path: std::path::PathBuf) -> Track {
    Track {
        id,
        path,
        title: format!("Track {id}"),
        artist: String::new(),
        album: String::new(),
        year: None,
        genre: None,
        duration_ms: 0,
        codec: "pcm".into(),
        sample_rate: 8_000,
        channels: 1,
        artwork_key: None,
    }
}

fn pcm_wav() -> Vec<u8> {
    let samples = [0i16, 8_000, -8_000, 0];
    let data_size = samples.len() * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36u32 + data_size as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&8_000u32.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_size as u32).to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

#[test]
fn output_converters_fill_silence_without_allocating() {
    let ring = HeapRb::<f32>::new(16);
    let (_, mut consumer) = ring.split();
    let flush = AtomicBool::new(false);
    let paused = AtomicBool::new(false);
    let mut samples = [1i16, 1i16, 1i16, 1i16];
    fill_i16(&mut samples, 2, &mut consumer, &flush, &paused);
    assert_eq!(samples, [0, 0, 0, 0]);
}

#[test]
fn full_output_ring_keeps_worker_playing() {
    let suffix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("yinqidao-backpressure-{suffix}.wav"));
    fs::write(&path, pcm_wav()).expect("wav");

    let (event_tx, _event_rx) = unbounded();
    let command_queue = Arc::new(CommandQueue::new());
    let ring = HeapRb::<f32>::new(2);
    let (producer, mut consumer) = ring.split();
    let tracks = Arc::new(RwLock::new(HashMap::new()));
    let snapshot = Arc::new(RwLock::new(PlayerSnapshot::default()));
    let flush = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let mut worker = AudioWorker::new(
        command_queue,
        event_tx,
        producer,
        tracks,
        snapshot,
        flush,
        paused,
        AudioWorkerConfig {
            output_rate: 8_000,
            eq: EqSettings::default(),
            spatial: SpatialSettings::default(),
        },
    );
    worker.decoder = Some(DecoderStream::open(&path).expect("decoder"));
    worker.state = PlaybackState::Playing;

    let draining = Arc::new(AtomicBool::new(true));
    let drain_flag = draining.clone();
    let drain_thread = thread::spawn(move || {
        while drain_flag.load(Ordering::Acquire) {
            while consumer.try_pop().is_some() {}
            thread::yield_now();
        }
    });
    let decoded = worker.decode_next();
    draining.store(false, Ordering::Release);
    drain_thread.join().expect("drain thread");
    assert!(decoded.expect("decode chunk"));
    assert_eq!(worker.state, PlaybackState::Playing);
    fs::remove_file(path).expect("cleanup");
}

#[test]
fn set_queue_reanchors_index_to_current_track() {
    let (mut worker, _consumer, _command_tx, _flush, _paused) = test_worker(8);
    worker.queue = Arc::new(vec![1, 2, 3]);
    worker.queue_index = 1;
    worker.current_track = Some(2);
    worker.state = PlaybackState::Playing;

    worker.handle_command(PlayerCommand::SetQueue(Arc::new(vec![2, 3])));

    assert_eq!(worker.current_track, Some(2));
    assert_eq!(worker.queue_index, 0);
    assert_eq!(worker.state, PlaybackState::Playing);
}

#[test]
fn queue_position_preserves_first_duplicate_semantics() {
    let (mut worker, _consumer, _command_tx, _flush, _paused) = test_worker(8);

    worker.handle_command(PlayerCommand::SetQueue(Arc::new(vec![7, 11, 7, 13])));

    assert_eq!(worker.queue_position(7), Some(0));
    assert_eq!(worker.queue_position(11), Some(1));
    assert_eq!(worker.queue_position(13), Some(3));
}

#[test]
fn volume_burst_keeps_latest_value_before_next_command() {
    let (mut worker, _consumer, command_tx, _flush, _paused) = test_worker(8);
    worker.state = PlaybackState::Playing;
    assert!(command_tx.push(PlayerCommand::SetVolume(0.1), can_coalesce_commands));
    assert!(command_tx.push(PlayerCommand::SetVolume(0.2), can_coalesce_commands));
    assert!(command_tx.push(PlayerCommand::Pause, can_coalesce_commands));

    assert!(worker.handle_commands());
    assert_eq!(worker.snapshot.read().expect("snapshot").volume, 0.2);
    assert_eq!(worker.state, PlaybackState::Paused);
}

#[test]
fn removing_current_track_stops_and_flushes_old_pcm() {
    let (mut worker, mut consumer, _command_tx, flush, paused) = test_worker(8);
    worker.queue = Arc::new(vec![1, 2, 3]);
    worker.queue_index = 1;
    worker.current_track = Some(2);
    worker.state = PlaybackState::Playing;
    worker.producer.try_push(0.75).expect("old pcm");
    worker.producer.try_push(-0.75).expect("old pcm");

    worker.handle_command(PlayerCommand::SetQueue(Arc::new(vec![1, 3])));

    assert_eq!(worker.current_track, None);
    assert_eq!(worker.queue_index, 0);
    assert_eq!(worker.state, PlaybackState::Stopped);
    assert!(flush.load(Ordering::Acquire));
    let mut output = [1.0f32, 1.0];
    fill_f32(
        &mut output,
        2,
        &mut consumer,
        flush.as_ref(),
        paused.as_ref(),
    );
    assert_eq!(output, [0.0, 0.0]);
}

#[test]
fn previous_after_current_removal_starts_at_new_queue_anchor() {
    let suffix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("yinqidao-previous-{suffix}.wav"));
    fs::write(&path, pcm_wav()).expect("wav");
    let (mut worker, _consumer, _command_tx, _flush, _paused) = test_worker(8);
    worker
        .tracks
        .write()
        .expect("tracks")
        .insert(3, test_track(3, path.clone()));
    worker.queue = Arc::new(vec![1, 2, 3]);
    worker.queue_index = 1;
    worker.current_track = Some(2);
    worker.state = PlaybackState::Playing;

    worker.handle_command(PlayerCommand::SetQueue(Arc::new(vec![3])));
    worker.handle_command(PlayerCommand::Previous);

    assert_eq!(worker.current_track, Some(3));
    assert_eq!(worker.queue_index, 0);
    assert_eq!(worker.state, PlaybackState::Playing);
    fs::remove_file(path).expect("cleanup");
}

#[test]
fn failed_next_keeps_index_anchored_to_current_track() {
    let suffix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let missing_path = std::env::temp_dir().join(format!("yinqidao-missing-next-{suffix}.wav"));
    let (mut worker, _consumer, _command_tx, _flush, _paused) = test_worker(8);
    worker
        .tracks
        .write()
        .expect("tracks")
        .insert(2, test_track(2, missing_path));
    worker.queue = Arc::new(vec![1, 2]);
    worker.queue_index = 0;
    worker.current_track = Some(1);
    worker.state = PlaybackState::Playing;

    worker.handle_command(PlayerCommand::Next);

    assert_eq!(worker.current_track, Some(1));
    assert_eq!(worker.queue_index, 0);
    assert_eq!(worker.state, PlaybackState::Error);
}

#[test]
fn next_at_end_stops_and_flushes_old_pcm() {
    let (mut worker, mut consumer, _command_tx, flush, paused) = test_worker(8);
    worker.queue = Arc::new(vec![1]);
    worker.queue_index = 0;
    worker.current_track = Some(1);
    worker.state = PlaybackState::Playing;
    worker.producer.try_push(0.5).expect("old pcm");
    worker.producer.try_push(-0.5).expect("old pcm");

    worker.handle_command(PlayerCommand::Next);

    assert_eq!(worker.current_track, Some(1));
    assert_eq!(worker.queue_index, 0);
    assert_eq!(worker.state, PlaybackState::Stopped);
    assert!(flush.load(Ordering::Acquire));
    let mut output = [1.0f32, 1.0];
    fill_f32(
        &mut output,
        2,
        &mut consumer,
        flush.as_ref(),
        paused.as_ref(),
    );
    assert_eq!(output, [0.0, 0.0]);
}

#[test]
fn pause_outputs_silence_without_dropping_pcm() {
    let (mut worker, mut consumer, _command_tx, flush, paused) = test_worker(8);
    worker.state = PlaybackState::Playing;
    worker.producer.try_push(0.6).expect("pcm left");
    worker.producer.try_push(0.4).expect("pcm right");

    worker.handle_command(PlayerCommand::Pause);
    assert_eq!(worker.state, PlaybackState::Paused);
    assert!(paused.load(Ordering::Acquire));

    // When paused, fill_f32 outputs silence (0.0) without clearing consumer
    let mut output = [1.0f32, 1.0];
    fill_f32(
        &mut output,
        2,
        &mut consumer,
        flush.as_ref(),
        paused.as_ref(),
    );
    assert_eq!(output, [0.0, 0.0]);

    // When unpaused, playback resumes seamlessly with the preserved samples
    worker.handle_command(PlayerCommand::Play);
    assert!(!paused.load(Ordering::Acquire));
    fill_f32(
        &mut output,
        2,
        &mut consumer,
        flush.as_ref(),
        paused.as_ref(),
    );
    assert_eq!(output, [0.6, 0.4]);
}

#[test]
fn restore_track_sets_queue_index_and_paused_state() {
    let (mut worker, _consumer, _command_tx, _flush, paused) = test_worker(8);
    let track_path = std::env::temp_dir().join("yinqidao_test_restore.wav");
    fs::write(&track_path, pcm_wav()).expect("write test wav");
    let track = test_track(42, track_path.clone());
    worker.tracks.write().unwrap().insert(42, track);
    worker.handle_command(PlayerCommand::SetQueue(Arc::new(vec![10, 42, 99])));

    worker.handle_command(PlayerCommand::RestoreTrack {
        track_id: 42,
        position: Duration::from_millis(50),
        play: false,
    });

    assert_eq!(worker.queue_index, 1);
    assert_eq!(worker.current_track, Some(42));
    assert_eq!(worker.state, PlaybackState::Paused);
    assert!(paused.load(Ordering::Acquire));
    let snapshot = worker.snapshot.read().unwrap().clone();
    assert_eq!(snapshot.state, PlaybackState::Paused);
    assert_eq!(snapshot.current_track.as_ref().map(|t| t.id), Some(42));

    let _ = fs::remove_file(track_path);
}
