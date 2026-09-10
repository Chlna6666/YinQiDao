use std::{
    f32::consts::PI,
    io,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use yinqidao_audio_spatial::{ListenerPose, Vec3};

use super::head_tracking::{HeadTrackingBridge, HeadTrackingEulerPose, HeadTrackingProvider};

const OPENTRACK_PACKET_VALUES: usize = 6;
const OPENTRACK_PACKET_BYTES: usize = OPENTRACK_PACKET_VALUES * std::mem::size_of::<f64>();
const OPENTRACK_POLL_IDLE: Duration = Duration::from_millis(2);

/// Coordinate/unit conversion applied to OpenTrack's raw `X/Y/Z/Yaw/Pitch/Roll` packet.
///
/// OpenTrack's UDP protocol transports six native-endian `double` values without a framing header.
/// The default conversion follows its usual UI convention: translation in centimetres and rotation
/// in degrees. Every scale/sign remains explicit so unusual tracker mappings can be adapted without
/// changing the spatial renderer's +X right / +Y up / +Z forward convention.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpenTrackUdpTransform {
    pub translation_scale_meters_per_unit: f32,
    pub rotation_scale_radians_per_unit: f32,
    pub position_sign: Vec3,
    pub yaw_sign: f32,
    pub pitch_sign: f32,
    pub roll_sign: f32,
}

impl OpenTrackUdpTransform {
    pub const fn centimeters_degrees() -> Self {
        Self {
            translation_scale_meters_per_unit: 0.01,
            rotation_scale_radians_per_unit: PI / 180.0,
            position_sign: Vec3::new(1.0, 1.0, 1.0),
            yaw_sign: 1.0,
            pitch_sign: 1.0,
            roll_sign: 1.0,
        }
    }

    pub fn map_raw(self, raw: [f64; OPENTRACK_PACKET_VALUES]) -> HeadTrackingEulerPose {
        let translation_scale = finite_or(
            self.translation_scale_meters_per_unit,
            Self::centimeters_degrees().translation_scale_meters_per_unit,
        );
        let rotation_scale = finite_or(
            self.rotation_scale_radians_per_unit,
            Self::centimeters_degrees().rotation_scale_radians_per_unit,
        );
        let signs = sanitize_signs(self.position_sign);
        let yaw_sign = sanitize_sign(self.yaw_sign);
        let pitch_sign = sanitize_sign(self.pitch_sign);
        let roll_sign = sanitize_sign(self.roll_sign);

        HeadTrackingEulerPose {
            position_meters: Vec3::new(
                raw[0] as f32 * translation_scale * signs.x,
                raw[1] as f32 * translation_scale * signs.y,
                raw[2] as f32 * translation_scale * signs.z,
            ),
            yaw_radians: raw[3] as f32 * rotation_scale * yaw_sign,
            pitch_radians: raw[4] as f32 * rotation_scale * pitch_sign,
            roll_radians: raw[5] as f32 * rotation_scale * roll_sign,
        }
    }
}

impl Default for OpenTrackUdpTransform {
    fn default() -> Self {
        Self::centimeters_degrees()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpenTrackUdpConfig {
    /// Address YinQiDao listens on. OpenTrack's UDP output defaults to port 4242.
    pub bind_addr: SocketAddr,
    pub transform: OpenTrackUdpTransform,
}

impl Default for OpenTrackUdpConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 4242)),
            transform: OpenTrackUdpTransform::default(),
        }
    }
}

/// Non-blocking OpenTrack "UDP over network" provider.
///
/// `poll_pose()` drains all currently queued datagrams and returns only the newest valid pose. This
/// is intentional: head tracking is a latest-state control signal and stale UDP samples must not form
/// a latency-growing queue ahead of the realtime spatial engine. Socket I/O belongs on an input/UI
/// service thread; the audio callback still consumes only the lock-free ListenerPose slot.
pub struct OpenTrackUdpProvider {
    socket: UdpSocket,
    transform: OpenTrackUdpTransform,
    last_sender: Option<SocketAddr>,
    accepted_packets: u64,
    rejected_packets: u64,
    last_io_error: Option<io::ErrorKind>,
}

impl OpenTrackUdpProvider {
    pub fn bind(config: OpenTrackUdpConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr)?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            transform: config.transform,
            last_sender: None,
            accepted_packets: 0,
            rejected_packets: 0,
            last_io_error: None,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn transform(&self) -> OpenTrackUdpTransform {
        self.transform
    }

    pub fn set_transform(&mut self, transform: OpenTrackUdpTransform) {
        self.transform = transform;
    }

    pub fn last_sender(&self) -> Option<SocketAddr> {
        self.last_sender
    }

    pub fn accepted_packets(&self) -> u64 {
        self.accepted_packets
    }

    pub fn rejected_packets(&self) -> u64 {
        self.rejected_packets
    }

    pub fn last_io_error(&self) -> Option<io::ErrorKind> {
        self.last_io_error
    }
}

impl HeadTrackingProvider for OpenTrackUdpProvider {
    fn poll_pose(&mut self) -> Option<ListenerPose> {
        let mut packet = [0_u8; 256];
        let mut newest = None;

        loop {
            match self.socket.recv_from(&mut packet) {
                Ok((length, sender)) => {
                    self.last_io_error = None;
                    let Some(raw) = decode_opentrack_packet(&packet[..length]) else {
                        self.rejected_packets = self.rejected_packets.saturating_add(1);
                        continue;
                    };
                    self.accepted_packets = self.accepted_packets.saturating_add(1);
                    self.last_sender = Some(sender);
                    newest = Some(self.transform.map_raw(raw).listener_pose());
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.last_io_error = Some(error.kind());
                    break;
                }
            }
        }

        newest
    }

    fn reset(&mut self) {
        self.last_sender = None;
        self.last_io_error = None;
        let mut packet = [0_u8; 256];
        loop {
            match self.socket.recv_from(&mut packet) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    }
}

/// Dedicated non-realtime poller for OpenTrack. Starting this service is an explicit user/UI action;
/// the worker owns the UDP socket and calibration bridge while the audio callback remains completely
/// unaware of sockets, threads and packet parsing. OpenTrack can emit around 250 updates/s, so an
/// idle 2 ms poll interval is sufficient to drain bursts while still publishing only the newest pose.
pub struct OpenTrackHeadTrackingService {
    stop: Arc<AtomicBool>,
    recenter_requested: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl OpenTrackHeadTrackingService {
    pub fn start(config: OpenTrackUdpConfig) -> io::Result<Self> {
        let provider = OpenTrackUdpProvider::bind(config)?;
        let stop = Arc::new(AtomicBool::new(false));
        let recenter_requested = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_recenter = Arc::clone(&recenter_requested);
        let worker = thread::Builder::new()
            .name("yinqidao-opentrack".to_string())
            .spawn(move || {
                let mut bridge = HeadTrackingBridge::new(provider);
                while !worker_stop.load(Ordering::Acquire) {
                    let published = bridge.poll_and_publish().is_some();
                    if worker_recenter.swap(false, Ordering::AcqRel)
                        && !bridge.recenter_to_last_pose()
                    {
                        // Preserve the request until at least one valid tracker packet has arrived.
                        worker_recenter.store(true, Ordering::Release);
                    }
                    if !published {
                        thread::sleep(OPENTRACK_POLL_IDLE);
                    }
                }
                bridge.reset();
            })?;
        Ok(Self {
            stop,
            recenter_requested,
            worker: Some(worker),
        })
    }

    pub fn is_running(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }

    /// Recenter on the latest valid OpenTrack pose without touching the audio callback or restarting
    /// the UDP socket. If no packet has arrived yet, the request remains pending until the first one.
    pub fn request_recenter(&self) {
        self.recenter_requested.store(true, Ordering::Release);
    }

    pub fn stop(mut self) {
        self.stop_worker();
    }

    fn stop_worker(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for OpenTrackHeadTrackingService {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

/// Process-global control facade used by settings/UI. The Mutex protects lifecycle operations only;
/// neither the realtime audio callback nor the OpenTrack polling loop ever locks it. This keeps the
/// convenience API separate from the lock-free ListenerPose transport consumed by SpatialEngine.
fn global_service_slot() -> &'static Mutex<Option<OpenTrackHeadTrackingService>> {
    static SERVICE: OnceLock<Mutex<Option<OpenTrackHeadTrackingService>>> = OnceLock::new();
    SERVICE.get_or_init(|| Mutex::new(None))
}

/// Start (or restart) the process-wide OpenTrack input service. An existing service is stopped before
/// binding the requested UDP endpoint so rebinding the default port cannot race the old socket.
pub fn start_opentrack_head_tracking(config: OpenTrackUdpConfig) -> io::Result<()> {
    let mut slot = lock_service_slot();
    if let Some(service) = slot.take() {
        service.stop();
    }
    *slot = Some(OpenTrackHeadTrackingService::start(config)?);
    Ok(())
}

/// Stop the process-wide OpenTrack service. Returns whether a live service object existed.
pub fn stop_opentrack_head_tracking() -> bool {
    let service = lock_service_slot().take();
    let Some(service) = service else {
        return false;
    };
    service.stop();
    true
}

/// Ask the process-wide service to use its latest valid tracker sample as the new neutral pose.
/// The request is asynchronous and never enters the realtime audio command path.
pub fn recenter_opentrack_head_tracking() -> bool {
    let slot = lock_service_slot();
    let Some(service) = slot.as_ref().filter(|service| service.is_running()) else {
        return false;
    };
    service.request_recenter();
    true
}

pub fn opentrack_head_tracking_running() -> bool {
    lock_service_slot()
        .as_ref()
        .is_some_and(OpenTrackHeadTrackingService::is_running)
}

fn lock_service_slot() -> std::sync::MutexGuard<'static, Option<OpenTrackHeadTrackingService>> {
    match global_service_slot().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[inline]
fn decode_opentrack_packet(packet: &[u8]) -> Option<[f64; OPENTRACK_PACKET_VALUES]> {
    if packet.len() != OPENTRACK_PACKET_BYTES {
        return None;
    }

    let values = std::array::from_fn(|index| {
        let start = index * std::mem::size_of::<f64>();
        let bytes: [u8; 8] = packet[start..start + 8].try_into().expect("fixed packet slice");
        f64::from_ne_bytes(bytes)
    });
    values.iter().all(|value| value.is_finite()).then_some(values)
}

#[inline]
fn sanitize_signs(signs: Vec3) -> Vec3 {
    Vec3::new(
        sanitize_sign(signs.x),
        sanitize_sign(signs.y),
        sanitize_sign(signs.z),
    )
}

#[inline]
fn sanitize_sign(value: f32) -> f32 {
    if !value.is_finite() || value == 0.0 {
        1.0
    } else if value < 0.0 {
        -1.0
    } else {
        1.0
    }
}

#[inline]
fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(values: [f64; OPENTRACK_PACKET_VALUES]) -> [u8; OPENTRACK_PACKET_BYTES] {
        let mut packet = [0_u8; OPENTRACK_PACKET_BYTES];
        for (index, value) in values.into_iter().enumerate() {
            let start = index * 8;
            packet[start..start + 8].copy_from_slice(&value.to_ne_bytes());
        }
        packet
    }

    #[test]
    fn parses_exact_six_double_udp_contract() {
        let raw = [12.0, -4.0, 35.0, 90.0, -30.0, 15.0];
        assert_eq!(decode_opentrack_packet(&packet(raw)), Some(raw));
        assert!(decode_opentrack_packet(&[0_u8; 47]).is_none());
        assert!(decode_opentrack_packet(&[0_u8; 49]).is_none());
    }

    #[test]
    fn rejects_non_finite_tracker_packets() {
        let raw = [0.0, 0.0, 0.0, f64::NAN, 0.0, 0.0];
        assert!(decode_opentrack_packet(&packet(raw)).is_none());
    }

    #[test]
    fn default_transform_maps_centimeters_and_degrees() {
        let pose = OpenTrackUdpTransform::default()
            .map_raw([25.0, -10.0, 50.0, 90.0, 30.0, 0.0]);
        assert!((pose.position_meters.x - 0.25).abs() < 1.0e-6);
        assert!((pose.position_meters.y + 0.10).abs() < 1.0e-6);
        assert!((pose.position_meters.z - 0.50).abs() < 1.0e-6);
        assert!((pose.yaw_radians - PI * 0.5).abs() < 1.0e-6);
        assert!((pose.pitch_radians - PI / 6.0).abs() < 1.0e-6);
    }

    #[test]
    fn axis_signs_can_adapt_tracker_coordinate_conventions() {
        let transform = OpenTrackUdpTransform {
            position_sign: Vec3::new(-1.0, 1.0, -1.0),
            yaw_sign: -1.0,
            pitch_sign: 1.0,
            roll_sign: -1.0,
            ..OpenTrackUdpTransform::default()
        };
        let pose = transform.map_raw([10.0, 20.0, 30.0, 45.0, 10.0, -15.0]);
        assert!((pose.position_meters.x + 0.10).abs() < 1.0e-6);
        assert!((pose.position_meters.y - 0.20).abs() < 1.0e-6);
        assert!((pose.position_meters.z + 0.30).abs() < 1.0e-6);
        assert!(pose.yaw_radians < 0.0);
        assert!(pose.roll_radians > 0.0);
    }
}