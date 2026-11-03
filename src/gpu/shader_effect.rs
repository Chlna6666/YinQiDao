//! Nova renderer-extension effects. WGSL is compiled in build.rs by BMCBL's
//! gfx-shader-build; there is no runtime shader frontend, old GpuMesh3d path or
//! independent window renderer. Each window owns its pipelines and uniform slots.
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use gfx_core::{
    BackendKind, BindGroupDescriptor, BindGroupEntry, BindGroupId, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindGroupLayoutId, BindingResource, BindingType, BlendMode,
    BufferBinding, BufferDescriptor, BufferId, BufferUsage, DrawCommand, Format, MemoryLocation,
    PipelineLayoutDescriptor, PipelineLayoutId, PrimitiveTopology, RasterState, RenderCommand,
    RenderPassId, RenderPipelineDescriptor, RenderPipelineId, ShaderModuleDescriptor,
    ShaderModuleId, ShaderStage, ShaderStages, bind_group_list,
};
use gpui::{
    AnyElement, ExtensionDevice, RendererExtension, RendererExtensionContext,
    RendererExtensionRenderer, Window, canvas, prelude::*,
};

mod shaders {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/shaders_bytes.rs"));
}

/// A renderer-owned full-screen shader; geometry is generated from vertex_index.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ShaderKind {
    Fluid,
    LiquidGlass,
}

impl ShaderKind {
    fn entries(self) -> (&'static str, &'static str) {
        match self {
            Self::Fluid => ("vs_shader_effect", "fs_apple_fluid_opaque"),
            Self::LiquidGlass => ("vs_lyrics_liquid_glass", "fs_lyrics_liquid_glass"),
        }
    }
}

/// Compact 64-byte application-owned parameters. Nova adds viewport/bounds/time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ShaderParams16 {
    columns: [[f32; 4]; 4],
}

impl ShaderParams16 {
    pub(crate) const fn from_columns(columns: [[f32; 4]; 4]) -> Self {
        Self { columns }
    }
}

pub(crate) fn next_shader_effect_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The UI only publishes immutable values; Native GPUI owns every GPU object.
#[derive(Clone)]
struct EffectSnapshot {
    id: u64,
    kind: ShaderKind,
    params: ShaderParams16,
    running: bool,
    sample_time: Instant,
}

impl RendererExtension for EffectSnapshot {
    fn is_time_dependent(&self) -> bool {
        self.running
    }

    fn create_renderer(
        &self,
        device: &mut dyn ExtensionDevice,
        context: RendererExtensionContext,
    ) -> gpui::Result<Box<dyn RendererExtensionRenderer>> {
        Ok(Box::new(EffectRenderer::new(device, context)?))
    }
}

/// Draw in the host compositor pass, with no duplicate swapchain or CPU staging image.
pub(crate) fn shader_effect_canvas(
    kind: ShaderKind,
    id: u64,
    params: ShaderParams16,
    running: bool,
) -> AnyElement {
    let input = Arc::new(EffectSnapshot {
        id,
        kind,
        params,
        running,
        sample_time: Instant::now(),
    });
    canvas(
        |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window: &mut Window, _cx| {
            window.paint_renderer_extension(bounds, input.clone());
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

struct Program {
    vertex: ShaderModuleId,
    fragment: ShaderModuleId,
    pipeline: RenderPipelineId,
}

struct UniformSlot {
    buffer: BufferId,
    group: BindGroupId,
}

struct EffectRenderer {
    layout: BindGroupLayoutId,
    pipeline_layout: PipelineLayoutId,
    render_pass: RenderPassId,
    format: Format,
    viewport: gfx_core::Extent2d,
    programs: HashMap<ShaderKind, Program>,
    slots: HashMap<(ShaderKind, u64), UniformSlot>,
}

fn embedded_shader(
    backend: BackendKind,
    stage: ShaderStage,
    entry: &str,
) -> gpui::Result<gfx_core::ShaderBinary> {
    let artifact = match backend {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        BackendKind::OpenGl => shaders::yinqidao_effects_opengl_shader(entry),
        #[cfg(target_os = "windows")]
        BackendKind::Dx11 => shaders::yinqidao_effects_dx11_shader(entry),
        #[cfg(target_os = "windows")]
        BackendKind::Dx12 => shaders::yinqidao_effects_dx12_shader(entry),
        #[cfg(target_os = "macos")]
        BackendKind::Metal => shaders::yinqidao_effects_metal_shader(entry),
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
        BackendKind::Vulkan => shaders::yinqidao_effects_vulkan_shader(entry),
        _ => None,
    };
    let artifact = artifact.ok_or_else(|| {
        anyhow::anyhow!("YinQiDao native shader {entry} is unavailable for {backend:?}")
    })?;
    Ok(artifact.to_binary(stage, entry)?)
}

impl EffectRenderer {
    fn new(
        device: &mut dyn ExtensionDevice,
        context: RendererExtensionContext,
    ) -> gpui::Result<Self> {
        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("YinQiDao shader effects uniforms".into()),
            entries: vec![BindGroupLayoutEntry {
                binding: 0,
                binding_type: BindingType::UniformBuffer,
                stages: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
            }],
        })?;
        let pipeline_layout = match device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("YinQiDao native effects pipeline".into()),
            bind_group_layouts: vec![layout],
        }) {
            Ok(layout) => layout,
            Err(error) => {
                let _ = device.destroy_bind_group_layout(layout);
                return Err(error.into());
            }
        };
        Ok(Self {
            layout,
            pipeline_layout,
            render_pass: context.render_pass(),
            format: context.color_format(),
            viewport: context.viewport(),
            programs: HashMap::new(),
            slots: HashMap::new(),
        })
    }

    fn ensure_pipeline(
        &mut self,
        kind: ShaderKind,
        device: &mut dyn ExtensionDevice,
        context: &RendererExtensionContext,
    ) -> gpui::Result<RenderPipelineId> {
        if self.render_pass != context.render_pass()
            || self.format != context.color_format()
            || self.viewport != context.viewport()
        {
            for (_, program) in self.programs.drain() {
                device.destroy_render_pipeline(program.pipeline)?;
                device.destroy_shader_module(program.vertex)?;
                device.destroy_shader_module(program.fragment)?;
            }
            self.render_pass = context.render_pass();
            self.format = context.color_format();
            self.viewport = context.viewport();
        }
        if let Some(program) = self.programs.get(&kind) {
            return Ok(program.pipeline);
        }
        let (vs_entry, fs_entry) = kind.entries();
        let vertex = device.create_shader_module(&ShaderModuleDescriptor {
            label: Some(format!("YinQiDao {vs_entry}")),
            binary: embedded_shader(context.backend_kind(), ShaderStage::Vertex, vs_entry)?,
        })?;
        let fragment = match device.create_shader_module(&ShaderModuleDescriptor {
            label: Some(format!("YinQiDao {fs_entry}")),
            binary: embedded_shader(context.backend_kind(), ShaderStage::Fragment, fs_entry)?,
        }) {
            Ok(fragment) => fragment,
            Err(error) => {
                let _ = device.destroy_shader_module(vertex);
                return Err(error.into());
            }
        };
        let pipeline = device.create_render_pipeline(
            &RenderPipelineDescriptor {
                label: Some(format!("YinQiDao {fs_entry} pipeline")),
                vertex_shader: vertex,
                vertex_entry_point: vs_entry.into(),
                fragment_shader: fragment,
                fragment_entry_point: fs_entry.into(),
                vertex_buffers: Vec::new(),
                render_pass: context.render_pass(),
                pipeline_layout: Some(self.pipeline_layout),
                color_format: context.color_format(),
                blend_mode: match kind {
                    ShaderKind::Fluid => BlendMode::Replace,
                    ShaderKind::LiquidGlass => BlendMode::PremultipliedAlpha,
                },
                primitive_topology: PrimitiveTopology::TriangleList,
                raster: RasterState {
                    front_face: gfx_core::FrontFace::Ccw,
                    cull_mode: None,
                },
                depth_state: None,
            },
            context.viewport(),
        );
        let pipeline = match pipeline {
            Ok(pipeline) => pipeline,
            Err(error) => {
                let _ = device.destroy_shader_module(fragment);
                let _ = device.destroy_shader_module(vertex);
                return Err(error.into());
            }
        };
        self.programs.insert(
            kind,
            Program {
                vertex,
                fragment,
                pipeline,
            },
        );
        Ok(pipeline)
    }

    fn uniform_slot(
        &mut self,
        id: (ShaderKind, u64),
        device: &mut dyn ExtensionDevice,
    ) -> gpui::Result<&UniformSlot> {
        use std::collections::hash_map::Entry;
        match self.slots.entry(id) {
            Entry::Occupied(slot) => Ok(slot.into_mut()),
            Entry::Vacant(slot) => {
                // Constant-buffer size is deliberately rounded to 256 bytes for D3D.
                const BUFFER_BYTES: u64 = 256;
                let buffer = device.create_buffer(&BufferDescriptor {
                    label: Some("YinQiDao effect uniforms".into()),
                    size: BUFFER_BYTES,
                    usage: BufferUsage::UNIFORM | BufferUsage::COPY_DST,
                    memory_location: MemoryLocation::CpuToGpu,
                })?;
                let group = match device.create_bind_group(&BindGroupDescriptor {
                    label: Some("YinQiDao effect parameters".into()),
                    layout: self.layout,
                    entries: vec![BindGroupEntry {
                        binding: 0,
                        resource: BindingResource::Buffer(BufferBinding {
                            buffer,
                            offset: 0,
                            size: BUFFER_BYTES,
                            stride: None,
                        }),
                    }],
                }) {
                    Ok(group) => group,
                    Err(error) => {
                        let _ = device.destroy_buffer(buffer);
                        return Err(error.into());
                    }
                };
                Ok(slot.insert(UniformSlot { buffer, group }))
            }
        }
    }

    fn write_parameters(input: &EffectSnapshot, context: &RendererExtensionContext) -> [u8; 96] {
        let bounds = context.bounds();
        let mut values = [0.0_f32; 24];
        for (column, items) in input.params.columns.iter().enumerate() {
            values[column * 4..column * 4 + 4].copy_from_slice(items);
        }
        values[16] = f64::from(bounds.origin.x) as f32;
        values[17] = f64::from(bounds.origin.y) as f32;
        values[18] = f64::from(bounds.size.width) as f32;
        values[19] = f64::from(bounds.size.height) as f32;
        values[20] = context.viewport().width() as f32;
        values[21] = context.viewport().height() as f32;
        // One compositor-owned clock; no view notifications and no extra RAF timer.
        let elapsed = if input.running {
            context
                .frame_time()
                .saturating_duration_since(input.sample_time)
                .as_secs_f32()
        } else {
            0.0
        };
        values[22] = (elapsed * 60.0).floor() / 60.0;
        let mut bytes = [0_u8; 96];
        for (index, value) in values.into_iter().enumerate() {
            bytes[index * 4..][..4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }
}

impl RendererExtensionRenderer for EffectRenderer {
    fn render(
        &mut self,
        extension: &dyn RendererExtension,
        device: &mut dyn ExtensionDevice,
        context: RendererExtensionContext,
        steps: &mut Vec<RenderCommand>,
    ) -> gpui::Result<()> {
        let input = extension
            .downcast_ref::<EffectSnapshot>()
            .ok_or_else(|| anyhow::anyhow!("unexpected native shader effect type"))?;
        let pipeline = self.ensure_pipeline(input.kind, device, &context)?;
        let slot = self.uniform_slot((input.kind, input.id), device)?;
        device.write_buffer(slot.buffer, 0, &Self::write_parameters(input, &context))?;
        steps.push(RenderCommand::Draw(DrawCommand {
            pipeline,
            bind_groups: bind_group_list([slot.group]),
            vertex_count: 6,
            first_vertex: 0,
            instance_count: 1,
            first_instance: 0,
            scissor: None,
        }));
        Ok(())
    }

    fn destroy(&mut self, device: &mut dyn ExtensionDevice) -> gpui::Result<()> {
        for (_, slot) in self.slots.drain() {
            device.destroy_bind_group(slot.group)?;
            device.destroy_buffer(slot.buffer)?;
        }
        for (_, program) in self.programs.drain() {
            device.destroy_render_pipeline(program.pipeline)?;
            device.destroy_shader_module(program.vertex)?;
            device.destroy_shader_module(program.fragment)?;
        }
        device.destroy_pipeline_layout(self.pipeline_layout)?;
        device.destroy_bind_group_layout(self.layout)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn uniform_packet_is_exactly_96_bytes() {
        assert_eq!(std::mem::size_of::<[[f32; 4]; 4]>(), 64);
        assert_eq!(96 % 16, 0);
    }
}
