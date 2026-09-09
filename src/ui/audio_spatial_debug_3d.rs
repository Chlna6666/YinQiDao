use std::f32::consts::PI;
use std::sync::{Arc, OnceLock};

use gpui::{
    GpuMesh3d, GpuMesh3dDrawParameters, GpuMesh3dDrawRanges, GpuMesh3dRange, GpuMesh3dShader,
    GpuMesh3dVertex, WgslShaderSource,
};
use yinqidao_audio_spatial::{
    DEFAULT_HEAD_RADIUS_M, SpatialDebugSnapshot, SpatialDebugSourceKind, Vec3,
    late_field_telemetry,
};

mod humanoid_generated {
    include!("audio_debug_humanoid_generated.rs");
}
use humanoid_generated::{HUMANOID_ASSET_READY, HUMANOID_INDICES, HUMANOID_VERTICES};

const SHADER_SOURCE: &str = include_str!("audio_spatial_debug_3d.wgsl");
const MIN_SCENE_RADIUS: f32 = 1.8;
const SOURCE_RADIUS: f32 = 0.075;
const EAR_RADIUS: f32 = 0.040;
const HEAD_RADIUS: f32 = DEFAULT_HEAD_RADIUS_M * 1.31;
const BOUNCE_RADIUS: f32 = 0.035;
const ROOM_EDGE_WIDTH: f32 = 0.010;
const LATE_FIELD_EDGE_WIDTH: f32 = 0.006;
const PATH_WIDTH: f32 = 0.008;
const DIRECT_EAR_PATH_WIDTH: f32 = 0.0045;
const VELOCITY_WIDTH: f32 = 0.010;
const LEFT_EAR_COLOR: [f32; 4] = [0.22, 0.62, 1.0, 1.0];
const RIGHT_EAR_COLOR: [f32; 4] = [1.0, 0.38, 0.32, 1.0];
const HUMANOID_COLOR: [f32; 4] = [0.18, 0.54, 0.64, 1.0];

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

    pub(crate) fn draw_parameters(
        &self,
        aspect: f32,
        camera: SpatialDebug3dCamera,
    ) -> GpuMesh3dDrawParameters {
        let aspect = aspect.max(0.1);
        let radius = self.fit_radius.max(MIN_SCENE_RADIUS);
        let model = mat4_scale([1.0 / radius, 1.0 / radius, 1.0 / radius]);
        let orbit_distance = (3.35 / camera.zoom.max(0.1)).clamp(1.45, 7.0);
        let horizontal = camera.pitch.cos();
        let eye = [
            camera.yaw.sin() * horizontal * orbit_distance,
            -camera.pitch.sin() * orbit_distance,
            camera.yaw.cos() * horizontal * orbit_distance,
        ];
        let view = mat4_look_at(eye, [0.0, -0.08, 0.0], [0.0, 1.0, 0.0]);
        let projection = mat4_perspective(aspect, 52.0_f32.to_radians(), 0.04, 64.0);
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
    let mut builder = MeshBuilder::default();
    let listener = snapshot.listener;
    let (right, up, forward) = listener.basis();
    let to_local = |position: Vec3| {
        let relative = position - listener.position;
        [relative.dot(right), relative.dot(up), relative.dot(forward)]
    };
    let (left_ear_world, right_ear_world) = listener.ear_positions();
    let left_ear = to_local(left_ear_world);
    let right_ear = to_local(right_ear_world);

    // Must match the audio-spatial image-source room geometry. The room is intentionally independent
    // of individual source positions so every authored channel shares one acoustic enclosure.
    let room = snapshot.environment.room_size.clamp(0.0, 1.0);
    let half_width = 1.65 + room * 3.00;
    let half_height = 1.25 + room * 1.50;
    let half_depth = 2.10 + room * 4.10;
    let fit_radius = half_width.max(half_depth).max(half_height).max(MIN_SCENE_RADIUS);

    builder.push_humanoid_listener(left_ear, right_ear);

    let source_count = snapshot.source_count.min(snapshot.sources.len());
    for source in snapshot.sources[..source_count].iter().copied() {
        if !source.active {
            continue;
        }
        let position = to_local(source.position);
        let color = source_color(source.kind, source.elevation_degrees, source.source_index);
        let scale = if matches!(source.kind, SpatialDebugSourceKind::Lfe) {
            1.18
        } else {
            0.92 + source.near_field_amount.clamp(0.0, 1.0) * 0.28
        };
        builder.push_virtual_speaker(
            position,
            scale,
            color,
            matches!(source.kind, SpatialDebugSourceKind::Lfe),
        );
    }
    let opaque_count = builder.indices.len() as u32;

    // Head orientation axes: forward is cyan, interaural axis is deliberately split blue/red.
    builder.push_segment(
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.55],
        0.013,
        [0.30, 0.92, 1.0, 0.70],
    );
    builder.push_segment(left_ear, right_ear, 0.008, [0.58, 0.68, 0.80, 0.36]);

    // Each virtual source feeds two different acoustic endpoints. Visualizing both paths is crucial:
    // the geometry is symmetric around the head, while delay/gain/pinna processing is not generally
    // equal for a non-frontal source.
    for source in snapshot.sources[..source_count].iter().copied() {
        if !source.active {
            continue;
        }
        let source_position = to_local(source.position);
        let (left_alpha, right_alpha) = binaural_path_alpha(source.left_gain, source.right_gain);
        let left_width = DIRECT_EAR_PATH_WIDTH * (0.75 + left_alpha * 0.70);
        let right_width = DIRECT_EAR_PATH_WIDTH * (0.75 + right_alpha * 0.70);
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

    builder.push_room_box(half_width, half_height, half_depth);
    builder.push_floor_grid(half_width, -half_height, half_depth);
    builder.push_ceiling_grid(half_width, half_height, half_depth);

    // Late diffuse energy has no discrete image-source position. Render it as concentric transparent
    // volumes around the listener rather than inventing extra bounce rays. The opacity comes from the
    // exact FDN wet parameter used by the realtime engine.
    let late = late_field_telemetry(snapshot.sample_rate, snapshot.environment);
    if late.active {
        builder.push_late_field_volume(
            half_width,
            half_height,
            half_depth,
            late.wet_gain,
            late.feedback_gain,
        );
    }

    for source in snapshot.sources[..source_count].iter().copied() {
        if !source.active || matches!(source.kind, SpatialDebugSourceKind::Lfe) {
            continue;
        }
        let start = to_local(source.position);
        let velocity_local = [
            source.velocity.dot(right),
            source.velocity.dot(up),
            source.velocity.dot(forward),
        ];
        let speed = length3(velocity_local);
        if speed > 0.005 {
            let scale = (0.20 / speed.max(0.001)).min(0.85);
            builder.push_segment(
                start,
                add3(start, mul3(velocity_local, scale)),
                VELOCITY_WIDTH,
                [0.96, 0.84, 0.32, 0.72],
            );
        }
    }

    let reflection_count = snapshot.reflection_count.min(snapshot.reflections.len());
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
        let source_position = to_local(source.position);
        let bounce = to_local(reflection.bounce_position);
        let energy = reflection.wet_contribution.clamp(0.0, 1.0);
        let alpha = (0.10 + energy.sqrt() * 0.74).clamp(0.10, 0.82);
        let path_color = wall_color(reflection.wall, alpha);
        builder.push_segment(source_position, bounce, PATH_WIDTH, path_color);

        // The reflection's image direction also arrives independently at both ears. Keep these much
        // fainter than the direct paths so a 7.1.4 bed remains readable even with 72 reflection taps.
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
                alpha * left_alpha * 0.55,
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
                alpha * right_alpha * 0.55,
            ],
        );
        builder.push_octahedron(
            bounce,
            BOUNCE_RADIUS,
            wall_color(reflection.wall, (alpha + 0.12).min(0.92)),
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

fn source_color(kind: SpatialDebugSourceKind, elevation_degrees: f32, index: u16) -> [f32; 4] {
    if matches!(kind, SpatialDebugSourceKind::Lfe) {
        return [0.92, 0.36, 0.32, 1.0];
    }
    if elevation_degrees > 18.0 {
        return [0.68, 0.52, 1.0, 1.0];
    }
    const PALETTE: [[f32; 3]; 6] = [
        [0.35, 0.86, 0.58],
        [0.30, 0.72, 1.00],
        [1.00, 0.68, 0.30],
        [0.94, 0.48, 0.70],
        [0.52, 0.78, 0.96],
        [0.72, 0.88, 0.36],
    ];
    let rgb = PALETTE[usize::from(index) % PALETTE.len()];
    [rgb[0], rgb[1], rgb[2], 1.0]
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

    fn push_humanoid_listener(&mut self, left_ear: [f32; 3], right_ear: [f32; 3]) {
        if HUMANOID_ASSET_READY && !HUMANOID_VERTICES.is_empty() && !HUMANOID_INDICES.is_empty() {
            self.push_static_mesh(HUMANOID_VERTICES, HUMANOID_INDICES, HUMANOID_COLOR);
            self.push_octahedron(left_ear, EAR_RADIUS, LEFT_EAR_COLOR);
            self.push_octahedron(right_ear, EAR_RADIUS, RIGHT_EAR_COLOR);
            return;
        }

        // Build-safe fallback used until tools/audio_debug/export_humanoid_cc0.py has generated the
        // selected Shingox CC0 mesh. The acoustic listener origin and ear markers already use the
        // exact runtime geometry, so replacing only this visual shell cannot alter the audio result.
        self.push_octahedron([0.0, 0.0, 0.0], HEAD_RADIUS, [0.22, 0.72, 0.82, 1.0]);
        self.push_segment(
            [0.0, -0.10, -0.015],
            [0.0, -0.24, -0.025],
            0.052,
            [0.20, 0.52, 0.62, 1.0],
        );
        self.push_segment(
            [0.0, -0.22, -0.03],
            [0.0, -0.58, -0.055],
            0.155,
            [0.17, 0.42, 0.52, 1.0],
        );
        self.push_segment(
            [-0.13, -0.25, -0.02],
            [-0.30, -0.49, -0.04],
            0.044,
            [0.16, 0.38, 0.48, 1.0],
        );
        self.push_segment(
            [0.13, -0.25, -0.02],
            [0.30, -0.49, -0.04],
            0.044,
            [0.16, 0.38, 0.48, 1.0],
        );
        self.push_octahedron(left_ear, EAR_RADIUS, LEFT_EAR_COLOR);
        self.push_octahedron(right_ear, EAR_RADIUS, RIGHT_EAR_COLOR);
        self.push_octahedron(
            [0.0, -0.005, HEAD_RADIUS * 0.92],
            0.026,
            [0.44, 0.86, 0.92, 1.0],
        );
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

    fn push_room_box(&mut self, half_width: f32, half_height: f32, half_depth: f32) {
        self.push_wire_box(
            half_width,
            half_height,
            half_depth,
            ROOM_EDGE_WIDTH,
            [0.33, 0.43, 0.56, 0.28],
        );
    }

    fn push_late_field_volume(
        &mut self,
        half_width: f32,
        half_height: f32,
        half_depth: f32,
        wet_gain: f32,
        feedback_gain: f32,
    ) {
        let base_alpha = (0.055 + wet_gain.clamp(0.0, 0.25) * 1.65).clamp(0.055, 0.26);
        let feedback_tint = ((feedback_gain - 0.50) / 0.32).clamp(0.0, 1.0);
        for (scale, alpha_scale) in [(0.38, 0.50), (0.60, 0.72), (0.82, 1.0)] {
            self.push_wire_box(
                half_width * scale,
                half_height * scale,
                half_depth * scale,
                LATE_FIELD_EDGE_WIDTH,
                [
                    0.30 + feedback_tint * 0.16,
                    0.42 + feedback_tint * 0.08,
                    0.96,
                    base_alpha * alpha_scale,
                ],
            );
        }
    }

    fn push_wire_box(
        &mut self,
        half_width: f32,
        half_height: f32,
        half_depth: f32,
        edge_width: f32,
        color: [f32; 4],
    ) {
        let p = [
            [-half_width, -half_height, -half_depth],
            [half_width, -half_height, -half_depth],
            [half_width, -half_height, half_depth],
            [-half_width, -half_height, half_depth],
            [-half_width, half_height, -half_depth],
            [half_width, half_height, -half_depth],
            [half_width, half_height, half_depth],
            [-half_width, half_height, half_depth],
        ];
        for (a, b) in [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ] {
            self.push_segment(p[a], p[b], edge_width, color);
        }
    }

    fn push_floor_grid(&mut self, half_width: f32, y: f32, half_depth: f32) {
        self.push_horizontal_grid(half_width, y, half_depth, [0.24, 0.32, 0.42, 0.14]);
    }

    fn push_ceiling_grid(&mut self, half_width: f32, y: f32, half_depth: f32) {
        self.push_horizontal_grid(half_width, y, half_depth, [0.22, 0.42, 0.50, 0.08]);
    }

    fn push_horizontal_grid(
        &mut self,
        half_width: f32,
        y: f32,
        half_depth: f32,
        color: [f32; 4],
    ) {
        let lines = 8usize;
        for index in 1..lines {
            let t = index as f32 / lines as f32;
            let x = -half_width + half_width * 2.0 * t;
            let z = -half_depth + half_depth * 2.0 * t;
            self.push_segment(
                [x, y, -half_depth],
                [x, y, half_depth],
                ROOM_EDGE_WIDTH * 0.40,
                color,
            );
            self.push_segment(
                [-half_width, y, z],
                [half_width, y, z],
                ROOM_EDGE_WIDTH * 0.40,
                color,
            );
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
