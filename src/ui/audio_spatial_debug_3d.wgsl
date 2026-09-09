struct SpatialDebugDrawParameters {
    bounds_origin: vec2<f32>,
    bounds_size: vec2<f32>,
    content_mask_origin: vec2<f32>,
    content_mask_size: vec2<f32>,
    view_proj_model: mat4x4<f32>,
};

struct MeshAnimation {
    property_and_flags: vec4<u32>,
    sampled: vec4<f32>,
};

struct GlobalParams {
    viewport_size: vec2<f32>,
    premultiplied_alpha: u32,
    pad: u32,
};

struct SpatialDebugVertex {
    position_x: f32,
    position_y: f32,
    position_z: f32,
    color_rgba8: u32,
};

struct SpatialDebugVarying {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) world_position: vec3<f32>,
    @location(2) @interpolate(flat) draw_bounds: vec4<f32>,
    @location(3) @interpolate(flat) animation_opacity: f32,
};

@group(0) @binding(0) var<uniform> globals: GlobalParams;
@group(0) @binding(20) var<storage, read> spatial_debug_draw_parameters: array<SpatialDebugDrawParameters>;
@group(0) @binding(21) var<storage, read> spatial_debug_vertices: array<SpatialDebugVertex>;
@group(0) @binding(22) var<storage, read> spatial_debug_animations: array<MeshAnimation>;

fn decode_color(encoded: u32) -> vec4<f32> {
    let red = f32(encoded & 0xffu) / 255.0;
    let green = f32((encoded >> 8u) & 0xffu) / 255.0;
    let blue = f32((encoded >> 16u) & 0xffu) / 255.0;
    let alpha_and_flags = (encoded >> 24u) & 0xffu;
    let alpha = f32(alpha_and_flags & 0x1fu) / 31.0;
    return vec4<f32>(red, green, blue, alpha);
}

fn animation_active(animation: MeshAnimation) -> bool {
    return animation.property_and_flags.y != 0u;
}

fn animation_opacity(animation: MeshAnimation) -> f32 {
    if (!animation_active(animation)) {
        return 1.0;
    }
    let property = animation.property_and_flags.x;
    if (property == 1u) {
        return animation.sampled.x;
    }
    if (property == 4u) {
        return animation.sampled.y;
    }
    return 1.0;
}

@vertex
fn vs_spatial_debug(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> SpatialDebugVarying {
    let vertex = spatial_debug_vertices[vertex_index];
    let draw_parameters = spatial_debug_draw_parameters[instance_index];
    let animation = spatial_debug_animations[instance_index];

    let base_bounds_origin = draw_parameters.bounds_origin;
    let base_bounds_size = draw_parameters.bounds_size;
    var bounds_origin = base_bounds_origin;
    var bounds_size = base_bounds_size;
    var content_origin = draw_parameters.content_mask_origin;
    var content_size = draw_parameters.content_mask_size;
    if (animation_active(animation)) {
        let property = animation.property_and_flags.x;
        if (property == 3u) {
            bounds_origin = bounds_origin + animation.sampled.xy;
        } else if (property == 2u || property == 4u) {
            let scale = animation.sampled.x;
            var pivot = base_bounds_origin + base_bounds_size * vec2<f32>(0.5, 0.5);
            if (property == 4u) {
                pivot = animation.sampled.zw;
            }
            bounds_origin = pivot + (bounds_origin - pivot) * scale;
            bounds_size = bounds_size * scale;
            content_origin = pivot + (content_origin - pivot) * scale;
            content_size = content_size * scale;
        }
    }

    let model_position = vec4<f32>(vertex.position_x, vertex.position_y, vertex.position_z, 1.0);
    let clip_position = draw_parameters.view_proj_model * model_position;
    let safe_w = select(min(clip_position.w, -0.0001), max(clip_position.w, 0.0001), clip_position.w >= 0.0);
    let ndc = clip_position.xyz / safe_w;

    let edge_inset = min(vec2<f32>(4.0, 4.0), bounds_size * vec2<f32>(0.04, 0.04));
    let mesh_origin = bounds_origin + edge_inset;
    let mesh_size = max(bounds_size - edge_inset * vec2<f32>(2.0, 2.0), vec2<f32>(1.0, 1.0));
    let draw_origin = max(mesh_origin, content_origin);
    let draw_max = min(mesh_origin + mesh_size, content_origin + content_size);
    let draw_size = max(draw_max - draw_origin, vec2<f32>(0.0, 0.0));
    let unit = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    let pixel_position = draw_origin + unit * draw_size;
    let viewport_size = max(globals.viewport_size, vec2<f32>(1.0));
    let device_position = pixel_position / viewport_size * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);

    var out: SpatialDebugVarying;
    out.position = vec4<f32>(device_position * clip_position.w, clip_position.z, clip_position.w);
    out.color = decode_color(vertex.color_rgba8);
    out.world_position = model_position.xyz;
    out.draw_bounds = vec4<f32>(draw_origin, draw_origin + draw_size);
    out.animation_opacity = animation_opacity(animation);
    return out;
}

fn inside_bounds(input: SpatialDebugVarying) -> bool {
    let p = input.position.xy;
    let b = input.draw_bounds;
    return p.x >= b.x && p.x <= b.z && p.y >= b.y && p.y <= b.w;
}

fn edge_alpha(input: SpatialDebugVarying) -> f32 {
    let p = input.position.xy;
    let b = input.draw_bounds;
    return clamp(min(min(p.x - b.x, b.z - p.x), min(p.y - b.y, b.w - p.y)), 0.0, 1.0);
}

@fragment
fn fs_spatial_debug(input: SpatialDebugVarying) -> @location(0) vec4<f32> {
    if (!inside_bounds(input)) {
        discard;
    }
    let dx = dpdx(input.world_position);
    let dy = dpdy(input.world_position);
    let normal_len = length(cross(dx, dy));
    var lighting = 1.0;
    if (normal_len > 0.00001) {
        let normal = normalize(cross(dx, dy));
        let key = normalize(vec3<f32>(0.45, 0.78, 0.42));
        lighting = 0.54 + 0.46 * abs(dot(normal, key));
    }
    let distance_fade = 1.0 / (1.0 + length(input.world_position) * 0.025);
    let alpha = input.color.a * edge_alpha(input) * input.animation_opacity;
    if (alpha <= 0.001) {
        discard;
    }
    let rgb = input.color.rgb * lighting * mix(0.82, 1.0, distance_fade);
    return vec4<f32>(rgb * alpha, alpha);
}
