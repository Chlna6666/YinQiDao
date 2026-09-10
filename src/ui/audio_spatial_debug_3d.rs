use std::f32::consts::PI;
use std::sync::{Arc, OnceLock};

use gpui::{
    GpuMesh3d, GpuMesh3dDrawParameters, GpuMesh3dDrawRanges, GpuMesh3dRange, GpuMesh3dShader,
    GpuMesh3dVertex, WgslShaderSource,
};
use yinqidao_audio_spatial::{
    DEFAULT_HEAD_RADIUS_M, MAX_DEBUG_SOURCES, SpatialDebugSnapshot, SpatialDebugSourceKind, Vec3,
    late_field_telemetry,
};

#[path = "audio_spatial_debug_sphere.rs"]
mod spherical_grid;
use spherical_grid::{bounded_fit_radius, direct_field_radius, for_each_spherical_segment};

#[path = "audio_spatial_debug_room.rs"]
mod room_reference;
use room_reference::{ProjectedRoomReference, RoomSegmentKind};

mod humanoid_generated {
    include!("audio_debug_humanoid_generated.rs");
}
use humanoid_generated::{HUMANOID_ASSET_READY, HUMANOID_INDICES, HUMANOID_VERTICES};

const SHADER_SOURCE: &str = include_str!("audio_spatial_debug_3d.wgsl");
const MIN_SCENE_RADIUS: f32 = 1.8;
const SOURCE_RADIUS: f32 = 0.075;
const LISTENER_HEAD_HALF_WIDTH: f32 = DEFAULT_HEAD_RADIUS_M * 0.94;
const LISTENER_HEAD_HALF_HEIGHT: f32 = 0.108;
const LISTENER_HEAD_HALF_DEPTH: f32 = 0.092;
const LISTENER_HEAD_LONGITUDE_SEGMENTS: usize = 20;
const LISTENER_HEAD_LATITUDE_SEGMENTS: usize = 13;
const EAR_HALF_WIDTH: f32 = 0.019;
const EAR_HALF_HEIGHT: f32 = 0.038;
const EAR_HALF_DEPTH: f32 = 0.025;
const EAR_LONGITUDE_SEGMENTS: usize = 12;
const EAR_LATITUDE_SEGMENTS: usize = 8;
const BOUNCE_RADIUS: f32 = 0.035;
const SPHERE_GRID_WIDTH: f32 = 0.0038;
const LATE_FIELD_GRID_WIDTH: f32 = 0.0028;
const ROOM_EDGE_WIDTH: f32 = 0.0022;
const ROOM_GRID_WIDTH: f32 = 0.0014;
const PATH_WIDTH: f32 = 0.008;
const DIRECT_EAR_PATH_WIDTH: f32 = 0.0045;
const VELOCITY_WIDTH: f32 = 0.010;
const LEFT_EAR_COLOR: [f32; 4] = [0.22, 0.62, 1.0, 1.0];
const RIGHT_EAR_COLOR: [f32; 4] = [1.0, 0.38, 0.32, 1.0];
const LISTENER_HEAD_COLOR: [f32; 4] = [0.18, 0.54, 0.64, 1.0];

#[derive(Clone, Copy, Debug)]
pub(crate) struct SpatialDebug3dCamera {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
}

impl Default for SpatialDebug3dCamera {
    fn default() -> Self {
        Self {
            yaw: 0.72,
            pitch: -0.34,
            zoom: 1.0,
        }
    }
}

impl SpatialDebug3dCamera {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn orbit(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.yaw = (self.yaw + yaw_delta).rem_euclid(PI * 2.0);
        self.pitch = (self.pitch + pitch_delta).clamp(-1.20, 1.20);
    }

    pub(crate) fn zoom_by(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(0.45, 2.6);
    }
}

pub(crate) struct SpatialDebug3dScene {
    mesh: Option<Arc<GpuMesh3d>>,
    sequence: u64,
    fit_radius: f32,
    error: Option<Arc<str>>,
}

impl Default for SpatialDebug3dScene {
    fn default() -> Self {
        Self {
            mesh: None,
            sequence: u64::MAX,
            fit_radius: MIN_SCENE_RADIUS,
            error: None,
        }
    }
}

impl SpatialDebug3dScene {
    pub(crate) fn clear(&mut self) {
        self.mesh = None;
        self.sequence = u64::MAX;
        self.fit_radius = MIN_SCENE_RADIUS;
        self.error = None;
    }

    pub(crate) fn update(&mut self, snapshot: Option<SpatialDebugSnapshot>) {
        let Some(snapshot) = snapshot else {
            self.clear();
            return;
        };
        if self.sequence == snapshot.sequence && self.mesh.is_some() {
            return;
        }

        match build_scene_mesh(snapshot, self.mesh.as_ref()) {
            Ok((mesh, fit_radius)) => {
                self.mesh = Some(mesh);
                self.sequence = snapshot.sequence;
                self.fit_radius = fit_radius.max(MIN_SCENE_RADIUS);
                self.error = None;
            }
            Err(error) => {
                self.error = Some(Arc::<str>::from(error));
                self.mesh = None;
                self.sequence = snapshot.sequence;
            }
        }
    }

    pub(crate) fn mesh(&self) -> Option<Arc<GpuMesh3d>> {
        self.mesh.clone()
    }

    pub(crate) fn error(&self) -> Option<Arc<str>> {
        self.error.clone()
    }

    pub(crate) fn fit_radius(&self) -> f32 {
        self.fit_radius.max(MIN_SCENE_RADIUS)
    }

    pub(crate) fn draw_parameters(
        &self,
        aspect: f32,
        camera: SpatialDebug3dCamera,
    ) -> GpuMesh3dDrawParameters {
        Self::draw_parameters_for(self.fit_radius(), aspect, camera)
    }

    pub(crate) fn draw_parameters_for(
        fit_radius: f32,
        aspect: f32,
        camera: SpatialDebug3dCamera,
    ) -> GpuMesh3dDrawParameters {
        let aspect = aspect.max(0.1);
        let radius = fit_radius.max(MIN_SCENE_RADIUS);
        let model = mat4_scale([1.0 / radius, 1.0 / radius, 1.0 / radius]);

        // Keep camera distance fixed and zoom by field-of-view. The spatial field is always centred
        // on the listener; room reflections are secondary paths and never redefine the camera axis.
        let orbit_distance = 3.35;
        let horizontal = camera.pitch.cos();
        let eye = [
            camera.yaw.sin() * horizontal * orbit_distance,
            -camera.pitch.sin() * orbit_distance,
            camera.yaw.cos() * horizontal * orbit_distance,
        ];
        let view = mat4_look_at(eye, [0.0, -0.08, 0.0], [0.0, 1.0, 0.0]);
        let vertical_fov = (52.0 / camera.zoom.max(0.1)).clamp(24.0, 78.0);
        let projection = mat4_perspective(aspect, vertical_fov.to_radians(), 0.04, 64.0);
        GpuMesh3dDrawParameters {
            view_projection_model: mat4_mul(projection, mat4_mul(view, model)),
        }
    }
}

fn spatial_debug_shader() -> Result<Arc<GpuMesh3dShader>, String> {
    static SHADER: OnceLock<Result<Arc<GpuMesh3dShader>, String>> = OnceLock::new();
    SHADER
        .get_or_init(|| {
            let source = WgslShaderSource::from_source(
                "src/ui/audio_spatial_debug_3d.wgsl",
                SHADER_SOURCE,
            )
            .map_err(|error| error.to_string())?;
            Ok(Arc::new(GpuMesh3dShader::new(
                Arc::new(source),
                "vs_spatial_debug",
                "fs_spatial_debug",
            )))
        })
        .clone()
}

fn build_scene_mesh(
    snapshot: SpatialDebugSnapshot,
    previous: Option<&Arc<GpuMesh3d>>,
) -> Result<(Arc<GpuMesh3d>, f32), String> {
    let shader = spatial_debug_shader()?;
    let mut builder = MeshBuilder::with_capacity();
    let listener = snapshot.listener;
    let (right, up, forward) = listener.basis();
    let to_local = |position: Vec3| {
        let relative = position - listener.position;
        [relative.dot(right), relative.dot(up), relative.dot(forward)]
    };
    let (left_ear_world, right_ear_world) = listener.ear_positions();
    let left_ear = to_local(left_ear_world);
    let right_ear = to_local(right_ear_world);
    let room_reference = (snapshot.environment.mix > 1.0e-5).then(|| {
        ProjectedRoomReference::from_fixed_world_room(listener, snapshot.environment)
    });

    let source_count = snapshot.source_count.min(snapshot.sources.len());
    let mut source_positions = [[0.0_f32; 3]; MAX_DEBUG_SOURCES];
    for source in snapshot.sources[..source_count].iter().copied() {
        let index = usize::from(source.source_index).min(MAX_DEBUG_SOURCES - 1);
        source_positions[index] = to_local(source.position);
    }
    let active_positions = snapshot.sources[..source_count]
        .iter()
        .filter(|source| source.active)
        .map(|source| {
            let index = usize::from(source.source_index).min(MAX_DEBUG_SOURCES - 1);
            &source_positions[index]
        });
    let field_radius = direct_field_radius(MIN_SCENE_RADIUS, active_positions);

    let reflection_count = snapshot.reflection_count.min(snapshot.reflections.len());
    let mut acoustic_extent = field_radius;
    for reflection in snapshot.reflections[..reflection_count].iter().copied() {
        if reflection.active {
            acoustic_extent = acoustic_extent.max(length3(to_local(reflection.bounce_position)));
        }
    }
    if let Some(room) = room_reference {
        acoustic_extent = acoustic_extent.max(room.max_extent());
    }
    let fit_radius = bounded_fit_radius(field_radius, acoustic_extent);

    builder.push_listener_head(left_ear, right_ear);
    for source in snapshot.sources[..source_count].iter().copied() {
        if !source.active {
            continue;
        }
        let position = source_positions[usize::from(source.source_index).min(MAX_DEBUG_SOURCES - 1)];
        let activity = source_activity_visual(source.input_peak, source.input_rms);
        let color = source_color(
            source.kind,
            source.elevation_degrees,
            source.source_index,
            activity,
        );
        let geometry_scale = if matches!(source.kind, SpatialDebugSourceKind::Lfe) {
            1.18
        } else {
            0.92 + source.near_field_amount.clamp(0.0, 1.0) * 0.28
        };
        let activity_scale = 0.62 + activity * 0.78;
        builder.push_virtual_speaker(
            position,
            geometry_scale * activity_scale,
            color,
            matches!(source.kind, SpatialDebugSourceKind::Lfe),
        );
    }
    let opaque_count = builder.indices.len() as u32;

    // Direct localization stays listener-centric and spherical. The rectangular room is drawn only
    // as a low-alpha diagnostic reference in the fixed world frame, projected through ListenerPose;
    // rotating/moving the listener therefore changes the view of the walls instead of dragging the
    // walls along with the head.
    builder.push_spherical_field(field_radius);
    builder.push_axis_guides(field_radius);
    if let Some(room) = room_reference {
        let room_alpha = (0.035 + snapshot.environment_contribution.clamp(0.0, 1.0) * 0.13)
            .clamp(0.035, 0.085);
        room.for_each_segment(|start, end, kind| {
            let (width, color) = match kind {
                RoomSegmentKind::Edge => (
                    ROOM_EDGE_WIDTH,
                    [0.54, 0.64, 0.76, room_alpha],
                ),
                RoomSegmentKind::FloorGrid => (
                    ROOM_GRID_WIDTH,
                    [0.96, 0.74, 0.30, room_alpha * 0.68],
                ),
                RoomSegmentKind::CeilingGrid => (
                    ROOM_GRID_WIDTH,
                    [0.44, 0.86, 0.96, room_alpha * 0.62],
                ),
            };
            builder.push_segment(start, end, width, color);
        });
    }
    builder.push_segment(left_ear, right_ear, 0.008, [0.58, 0.68, 0.80, 0.36]);

    for source in snapshot.sources[..source_count].iter().copied() {
        if !source.active {
            continue;
        }
        let source_position = source_positions
            [usize::from(source.source_index).min(MAX_DEBUG_SOURCES - 1)];
        let activity = source_activity_visual(source.input_peak, source.input_rms);
        let activity_alpha = 0.16 + activity * 0.84;
        let (left_alpha, right_alpha) = binaural_path_alpha(source.left_gain, source.right_gain);
        let left_alpha = left_alpha * activity_alpha;
        let right_alpha = right_alpha * activity_alpha;
        let left_width = DIRECT_EAR_PATH_WIDTH * (0.72 + activity * 0.52 + left_alpha * 0.38);
        let right_width = DIRECT_EAR_PATH_WIDTH * (0.72 + activity * 0.52 + right_alpha * 0.38);
        builder.push_segment(
            source_position,
            left_ear,
            left_width,
            [LEFT_EAR_COLOR[0], LEFT_EAR_COLOR[1], LEFT_EAR_COLOR[2], left_alpha],
        );
        builder.push_segment(
            source_position,
            right_ear,
            right_width,
            [
                RIGHT_EAR_COLOR[0],
                RIGHT_EAR_COLOR[1],
                RIGHT_EAR_COLOR[2],
                right_alpha,
            ],
        );
    }

    let late = late_field_telemetry(snapshot.sample_rate, snapshot.environment);
    if late.active {
        builder.push_late_field_shell(field_radius, late.wet_gain, late.feedback_gain);
    }

    for source in snapshot.sources[..source_count].iter().copied() {
        if !source.active || matches!(source.kind, SpatialDebugSourceKind::Lfe) {
            continue;
        }
        let start = source_positions[usize::from(source.source_index).min(MAX_DEBUG_SOURCES - 1)];
        let velocity_local = [
            source.velocity.dot(right),
            source.velocity.dot(up),
            source.velocity.dot(forward),
        ];
        let speed = length3(velocity_local);
        if speed > 0.005 {
            let scale = (0.20 / speed.max(0.001)).min(0.85);
            let activity = source_activity_visual(source.input_peak, source.input_rms);
            builder.push_segment(
                start,
                add3(start, mul3(velocity_local, scale)),
                VELOCITY_WIDTH,
                [0.96, 0.84, 0.32, 0.28 + activity * 0.52],
            );
        }
    }

    // The faint projected box shows where the world-fixed room surfaces actually are; the stronger
    // paths below remain the authoritative image-source source→bounce→ear geometry.
    for reflection in snapshot.reflections[..reflection_count].iter().copied() {
        if !reflection.active {
            continue;
        }
        let source_index = usize::from(reflection.source_index);
        let Some(source) = snapshot
            .sources
            .get(source_index)
            .copied()
            .filter(|source| source.active)
        else {
            continue;
        };
        let source_position = source_positions[source_index.min(MAX_DEBUG_SOURCES - 1)];
        let bounce = to_local(reflection.bounce_position);
        let activity = source_activity_visual(source.input_peak, source.input_rms);
        let energy = (reflection.wet_contribution.clamp(0.0, 1.0) * activity).clamp(0.0, 1.0);
        let alpha = (0.025 + energy.sqrt() * 0.56).clamp(0.025, 0.62);
        let path_color = wall_color(reflection.wall, alpha);
        builder.push_segment(source_position, bounce, PATH_WIDTH, path_color);

        let (left_alpha, right_alpha) =
            binaural_path_alpha(reflection.left_gain, reflection.right_gain);
        builder.push_segment(
            bounce,
            left_ear,
            PATH_WIDTH * 0.72,
            [
                path_color[0] * 0.72,
                path_color[1] * 0.82,
                1.0,
                alpha * left_alpha * 0.46,
            ],
        );
        builder.push_segment(
            bounce,
            right_ear,
            PATH_WIDTH * 0.72,
            [
                1.0,
                path_color[1] * 0.76,
                path_color[2] * 0.72,
                alpha * right_alpha * 0.46,
            ],
        );
        builder.push_octahedron(
            bounce,
            BOUNCE_RADIUS * (0.70 + activity * 0.45),
            wall_color(reflection.wall, (alpha + 0.10).min(0.76)),
        );
    }

    let total_count = builder.indices.len() as u32;
    let ranges = GpuMesh3dDrawRanges {
        opaque: GpuMesh3dRange {
            start: 0,
            count: opaque_count,
        },
        glass: GpuMesh3dRange {
            start: opaque_count,
            count: total_count.saturating_sub(opaque_count),
        },
        water: GpuMesh3dRange::default(),
    };
    let vertices: Arc<[GpuMesh3dVertex]> = Arc::from(builder.vertices);
    let indices: Arc<[u32]> = Arc::from(builder.indices);

    let mesh = if let Some(previous) = previous {
        Arc::new(GpuMesh3d {
            id: previous.id,
            generation: previous.generation.wrapping_add(1),
            vertices,
            indices,
            ranges,
            center: [0.0, 0.0, 0.0],
            fit_scale: 1.0 / fit_radius,
            vertical_scale: 1.0,
            shader,
        })
    } else {
        Arc::new(GpuMesh3d::new(
            vertices,
            indices,
            ranges,
            [0.0, 0.0, 0.0],
            1.0 / fit_radius,
            1.0,
            shader,
        ))
    };
    Ok((mesh, fit_radius))
}

fn source_activity_visual(peak: f32, rms: f32) -> f32 {
    let peak = linear_dbfs(peak);
    let rms = linear_dbfs(rms);
    let peak_normalized = ((peak + 48.0) / 48.0).clamp(0.0, 1.0);
    let rms_normalized = ((rms + 60.0) / 54.0).clamp(0.0, 1.0);
    (rms_normalized * 0.72 + peak_normalized * 0.28).clamp(0.0, 1.0)
}

fn linear_dbfs(value: f32) -> f32 {
    let value = if value.is_finite() { value.abs() } else { 0.0 };
    if value <= 1.0e-9 {
        -180.0
    } else {
        20.0 * value.log10()
    }
}

fn binaural_path_alpha(left_gain: f32, right_gain: f32) -> (f32, f32) {
    let left = if left_gain.is_finite() {
        left_gain.max(0.0)
    } else {
        0.0
    };
    let right = if right_gain.is_finite() {
        right_gain.max(0.0)
    } else {
        0.0
    };
    let maximum = left.max(right).max(1.0e-5);
    (
        (0.16 + 0.68 * (left / maximum).sqrt()).clamp(0.16, 0.84),
        (0.16 + 0.68 * (right / maximum).sqrt()).clamp(0.16, 0.84),
    )
}

fn source_color(
    kind: SpatialDebugSourceKind,
    elevation_degrees: f32,
    index: u16,
    activity: f32,
) -> [f32; 4] {
    let brightness = 0.34 + activity.clamp(0.0, 1.0) * 0.66;
    let base = if matches!(kind, SpatialDebugSourceKind::Lfe) {
        [0.92, 0.36, 0.32]
    } else if elevation_degrees > 18.0 {
        [0.68, 0.52, 1.0]
    } else if elevation_degrees < -18.0 {
        [0.36, 0.78, 0.96]
    } else {
        const PALETTE: [[f32; 3]; 6] = [
            [0.35, 0.86, 0.58],
            [0.30, 0.72, 1.00],
            [1.00, 0.68, 0.30],
            [0.94, 0.48, 0.70],
            [0.52, 0.78, 0.96],
            [0.72, 0.88, 0.36],
        ];
        PALETTE[usize::from(index) % PALETTE.len()]
    };
    [
        base[0] * brightness,
        base[1] * brightness,
        base[2] * brightness,
        1.0,
    ]
}

fn wall_color(
    wall: yinqidao_audio_spatial::SpatialDebugReflectionWall,
    alpha: f32,
) -> [f32; 4] {
    use yinqidao_audio_spatial::SpatialDebugReflectionWall;
    match wall {
        SpatialDebugReflectionWall::Left => [0.38, 0.72, 1.0, alpha],
        SpatialDebugReflectionWall::Right => [1.0, 0.52, 0.42, alpha],
        SpatialDebugReflectionWall::Front => [0.46, 0.92, 0.62, alpha],
        SpatialDebugReflectionWall::Rear => [0.86, 0.58, 1.0, alpha],
        SpatialDebugReflectionWall::Floor => [0.96, 0.74, 0.30, alpha],
        SpatialDebugReflectionWall::Ceiling => [0.44, 0.86, 0.96, alpha],
    }
}

#[derive(Default)]
struct MeshBuilder {
    vertices: Vec<GpuMesh3dVertex>,
    indices: Vec<u32>,
}

impl MeshBuilder {
    fn with_capacity() -> Self {
        Self {
            vertices: Vec::with_capacity(8_192),
            indices: Vec::with_capacity(36_864),
        }
    }

    fn push_vertex(&mut self, position: [f32; 3], color: [f32; 4]) -> u32 {
        let index = self.vertices.len().min(u32::MAX as usize) as u32;
        self.vertices.push(GpuMesh3dVertex { position, color });
        index
    }

    fn push_static_mesh(&mut self, vertices: &[[f32; 3]], indices: &[u32], color: [f32; 4]) {
        let base = self.vertices.len().min(u32::MAX as usize) as u32;
        for position in vertices.iter().copied() {
            self.push_vertex(position, color);
        }
        let vertex_count = vertices.len().min(u32::MAX as usize) as u32;
        for triangle in indices.chunks_exact(3) {
            if triangle.iter().all(|index| *index < vertex_count) {
                self.indices.extend([
                    base + triangle[0],
                    base + triangle[1],
                    base + triangle[2],
                ]);
            }
        }
    }

    fn push_listener_head(&mut self, left_ear: [f32; 3], right_ear: [f32; 3]) {
        if HUMANOID_ASSET_READY && !HUMANOID_VERTICES.is_empty() && !HUMANOID_INDICES.is_empty() {
            self.push_static_mesh(HUMANOID_VERTICES, HUMANOID_INDICES, LISTENER_HEAD_COLOR);
        } else {
            self.push_ellipsoid(
                [0.0, 0.0, 0.0],
                [
                    LISTENER_HEAD_HALF_WIDTH,
                    LISTENER_HEAD_HALF_HEIGHT,
                    LISTENER_HEAD_HALF_DEPTH,
                ],
                LISTENER_HEAD_LONGITUDE_SEGMENTS,
                LISTENER_HEAD_LATITUDE_SEGMENTS,
                LISTENER_HEAD_COLOR,
            );
            self.push_ellipsoid(
                [0.0, -0.004, LISTENER_HEAD_HALF_DEPTH * 0.93],
                [0.016, 0.024, 0.027],
                10,
                6,
                [0.28, 0.68, 0.76, 1.0],
            );
        }

        self.push_ellipsoid(
            left_ear,
            [EAR_HALF_WIDTH, EAR_HALF_HEIGHT, EAR_HALF_DEPTH],
            EAR_LONGITUDE_SEGMENTS,
            EAR_LATITUDE_SEGMENTS,
            LEFT_EAR_COLOR,
        );
        self.push_ellipsoid(
            right_ear,
            [EAR_HALF_WIDTH, EAR_HALF_HEIGHT, EAR_HALF_DEPTH],
            EAR_LONGITUDE_SEGMENTS,
            EAR_LATITUDE_SEGMENTS,
            RIGHT_EAR_COLOR,
        );
    }

    fn push_spherical_field(&mut self, radius: f32) {
        for_each_spherical_segment(radius, |start, end, equator| {
            let color = if equator {
                [0.32, 0.70, 0.88, 0.20]
            } else {
                [0.30, 0.46, 0.62, 0.105]
            };
            self.push_segment(start, end, SPHERE_GRID_WIDTH, color);
        });
    }

    fn push_late_field_shell(&mut self, radius: f32, wet_gain: f32, feedback_gain: f32) {
        let alpha = (0.018 + wet_gain.clamp(0.0, 0.25) * 0.70).clamp(0.018, 0.12);
        let feedback = ((feedback_gain - 0.50) / 0.32).clamp(0.0, 1.0);
        let shell_radius = radius * (0.78 + feedback * 0.10);
        for_each_spherical_segment(shell_radius, |start, end, equator| {
            let emphasis = if equator { 1.15 } else { 1.0 };
            self.push_segment(
                start,
                end,
                LATE_FIELD_GRID_WIDTH,
                [0.40 + feedback * 0.12, 0.48, 1.0, alpha * emphasis],
            );
        });
    }

    fn push_axis_guides(&mut self, radius: f32) {
        self.push_segment(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, radius * 0.34],
            0.010,
            [0.30, 0.92, 1.0, 0.72],
        );
        self.push_segment(
            [0.0, 0.0, 0.0],
            [radius * 0.24, 0.0, 0.0],
            0.006,
            [0.92, 0.52, 0.42, 0.32],
        );
        self.push_segment(
            [0.0, 0.0, 0.0],
            [0.0, radius * 0.24, 0.0],
            0.006,
            [0.66, 0.54, 1.0, 0.34],
        );
    }

    fn push_ellipsoid(
        &mut self,
        center: [f32; 3],
        radii: [f32; 3],
        longitude_segments: usize,
        latitude_segments: usize,
        color: [f32; 4],
    ) {
        let longitude_segments = longitude_segments.max(3);
        let latitude_segments = latitude_segments.max(2);
        let top = self.push_vertex(
            [center[0], center[1] + radii[1], center[2]],
            color,
        );
        let first_ring = self.vertices.len().min(u32::MAX as usize) as u32;

        for latitude in 1..latitude_segments {
            let theta = PI * latitude as f32 / latitude_segments as f32;
            let sin_theta = theta.sin();
            let cos_theta = theta.cos();
            for longitude in 0..longitude_segments {
                let phi = PI * 2.0 * longitude as f32 / longitude_segments as f32;
                let (sin_phi, cos_phi) = phi.sin_cos();
                self.push_vertex(
                    [
                        center[0] + radii[0] * sin_theta * cos_phi,
                        center[1] + radii[1] * cos_theta,
                        center[2] + radii[2] * sin_theta * sin_phi,
                    ],
                    color,
                );
            }
        }

        let bottom = self.push_vertex(
            [center[0], center[1] - radii[1], center[2]],
            color,
        );
        let longitude_u32 = longitude_segments.min(u32::MAX as usize) as u32;
        for longitude in 0..longitude_segments {
            let current = longitude.min(u32::MAX as usize) as u32;
            let next = ((longitude + 1) % longitude_segments).min(u32::MAX as usize) as u32;
            self.indices.extend([top, first_ring + current, first_ring + next]);
        }

        let ring_count = latitude_segments - 1;
        for ring in 0..ring_count.saturating_sub(1) {
            let current_ring = first_ring + ring.min(u32::MAX as usize) as u32 * longitude_u32;
            let next_ring = current_ring + longitude_u32;
            for longitude in 0..longitude_segments {
                let current = longitude.min(u32::MAX as usize) as u32;
                let next = ((longitude + 1) % longitude_segments).min(u32::MAX as usize) as u32;
                let a = current_ring + current;
                let b = current_ring + next;
                let c = next_ring + current;
                let d = next_ring + next;
                self.indices.extend([a, c, b, b, c, d]);
            }
        }

        let last_ring = first_ring
            + ring_count.saturating_sub(1).min(u32::MAX as usize) as u32 * longitude_u32;
        for longitude in 0..longitude_segments {
            let current = longitude.min(u32::MAX as usize) as u32;
            let next = ((longitude + 1) % longitude_segments).min(u32::MAX as usize) as u32;
            self.indices.extend([last_ring + next, last_ring + current, bottom]);
        }
    }

    fn push_virtual_speaker(
        &mut self,
        position: [f32; 3],
        scale: f32,
        color: [f32; 4],
        lfe: bool,
    ) {
        let radius = SOURCE_RADIUS * scale;
        self.push_octahedron(position, radius, color);
        let toward_listener = normalize3(mul3(position, -1.0));
        let nose = add3(
            position,
            mul3(toward_listener, radius * if lfe { 0.65 } else { 1.20 }),
        );
        self.push_segment(
            position,
            nose,
            radius * 0.24,
            [color[0], color[1], color[2], 0.92],
        );
        if !lfe {
            let halo = add3(position, mul3(toward_listener, -radius * 0.48));
            self.push_octahedron(
                halo,
                radius * 0.42,
                [color[0], color[1], color[2], 0.72],
            );
        }
    }

    fn push_octahedron(&mut self, center: [f32; 3], radius: f32, color: [f32; 4]) {
        let points = [
            add3(center, [radius, 0.0, 0.0]),
            add3(center, [-radius, 0.0, 0.0]),
            add3(center, [0.0, radius, 0.0]),
            add3(center, [0.0, -radius, 0.0]),
            add3(center, [0.0, 0.0, radius]),
            add3(center, [0.0, 0.0, -radius]),
        ];
        let base = self.vertices.len().min(u32::MAX as usize) as u32;
        for point in points {
            self.push_vertex(point, color);
        }
        const FACES: [[u32; 3]; 8] = [
            [2, 0, 4],
            [2, 4, 1],
            [2, 1, 5],
            [2, 5, 0],
            [3, 4, 0],
            [3, 1, 4],
            [3, 5, 1],
            [3, 0, 5],
        ];
        for face in FACES {
            self.indices.extend(face.map(|index| base + index));
        }
    }

    fn push_segment(
        &mut self,
        start: [f32; 3],
        end: [f32; 3],
        half_width: f32,
        color: [f32; 4],
    ) {
        let direction = sub3(end, start);
        let length = length3(direction);
        if length <= 1.0e-5 {
            return;
        }
        let direction = mul3(direction, length.recip());
        let helper = if direction[1].abs() < 0.90 {
            [0.0, 1.0, 0.0]
        } else {
            [1.0, 0.0, 0.0]
        };
        let side = mul3(normalize3(cross3(direction, helper)), half_width);
        let up = mul3(normalize3(cross3(side, direction)), half_width);
        let corners = [
            add3(add3(start, side), up),
            add3(sub3(start, side), up),
            sub3(sub3(start, side), up),
            add3(sub3(start, up), side),
            add3(add3(end, side), up),
            add3(sub3(end, side), up),
            sub3(sub3(end, side), up),
            add3(sub3(end, up), side),
        ];
        self.push_box_vertices(corners, color);
    }

    fn push_box_vertices(&mut self, corners: [[f32; 3]; 8], color: [f32; 4]) {
        let base = self.vertices.len().min(u32::MAX as usize) as u32;
        for position in corners {
            self.push_vertex(position, color);
        }
        const TRIANGLES: [[u32; 3]; 12] = [
            [0, 1, 2],
            [0, 2, 3],
            [4, 6, 5],
            [4, 7, 6],
            [0, 4, 5],
            [0, 5, 1],
            [1, 5, 6],
            [1, 6, 2],
            [2, 6, 7],
            [2, 7, 3],
            [3, 7, 4],
            [3, 4, 0],
        ];
        for triangle in TRIANGLES {
            self.indices.extend(triangle.map(|index| base + index));
        }
    }
}

fn mat4_scale(scale: [f32; 3]) -> [[f32; 4]; 4] {
    [
        [scale[0], 0.0, 0.0, 0.0],
        [0.0, scale[1], 0.0, 0.0],
        [0.0, 0.0, scale[2], 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mat4_perspective(aspect: f32, vertical_fov: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let focal = 1.0 / (vertical_fov * 0.5).tan().max(0.0001);
    let range = near - far;
    [
        [focal / aspect, 0.0, 0.0, 0.0],
        [0.0, focal, 0.0, 0.0],
        [0.0, 0.0, (far + near) / range, -1.0],
        [0.0, 0.0, (2.0 * far * near) / range, 0.0],
    ]
}

fn mat4_look_at(eye: [f32; 3], target: [f32; 3], world_up: [f32; 3]) -> [[f32; 4]; 4] {
    let forward = normalize3(sub3(target, eye));
    let right = normalize3(cross3(forward, world_up));
    let up = cross3(right, forward);
    [
        [right[0], up[0], -forward[0], 0.0],
        [right[1], up[1], -forward[1], 0.0],
        [right[2], up[2], -forward[2], 0.0],
        [-dot3(right, eye), -dot3(up, eye), dot3(forward, eye), 1.0],
    ]
}

fn mat4_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut result = [[0.0_f32; 4]; 4];
    for column in 0..4 {
        for row in 0..4 {
            result[column][row] = a[0][row] * b[column][0]
                + a[1][row] * b[column][1]
                + a[2][row] * b[column][2]
                + a[3][row] * b[column][3];
        }
    }
    result
}

fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn mul3(value: [f32; 3], scalar: f32) -> [f32; 3] {
    [value[0] * scalar, value[1] * scalar, value[2] * scalar]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length3(value: [f32; 3]) -> f32 {
    dot3(value, value).sqrt()
}

fn normalize3(value: [f32; 3]) -> [f32; 3] {
    let length = length3(value);
    if length <= 1.0e-6 {
        [0.0, 0.0, 1.0]
    } else {
        mul3(value, length.recip())
    }
}
