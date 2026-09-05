use std::sync::Arc;

use gpui::{
    AnyElement, GpuMesh3d, GpuMesh3dDrawParameters, GpuMesh3dDrawRanges, GpuMesh3dRange,
    GpuMesh3dShader, GpuMesh3dVertex, WgslShaderSource, canvas, prelude::*,
};

/// Standard 16-float payload for application-owned fullscreen WGSL effects.
///
/// GPUI's current custom-mesh ABI already exposes one `mat4x4<f32>` per draw. YinQiDao treats
/// that storage as four generic `vec4<f32>` columns rather than a transform matrix, which keeps
/// effect-specific state at 64 bytes without changing the renderer resource layout.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ShaderParams16 {
    columns: [[f32; 4]; 4],
}

impl ShaderParams16 {
    pub(crate) const fn from_columns(columns: [[f32; 4]; 4]) -> Self {
        Self { columns }
    }

    const fn as_draw_parameters(self) -> GpuMesh3dDrawParameters {
        GpuMesh3dDrawParameters {
            view_projection_model: self.columns,
        }
    }
}

/// Validated WGSL program plus the GPU-resident fullscreen quad used by 2D shader effects.
///
/// The WGSL source is validated once by GPUI/Naga. Nova then caches the backend render pipeline
/// by `GpuMesh3dShaderId`, while the quad's vertex/index buffers remain resident across frames.
#[derive(Clone)]
pub(crate) struct ShaderEffectProgram {
    mesh: Arc<GpuMesh3d>,
}

impl ShaderEffectProgram {
    pub(crate) fn from_source(
        label: impl Into<String>,
        source: impl Into<String>,
        vertex_entry_point: impl Into<String>,
        fragment_entry_point: impl Into<String>,
    ) -> Result<Arc<Self>, String> {
        let source =
            WgslShaderSource::from_source(label, source).map_err(|error| error.to_string())?;
        let shader = Arc::new(GpuMesh3dShader::new(
            Arc::new(source),
            vertex_entry_point,
            fragment_entry_point,
        ));

        let vertices = vec![
            GpuMesh3dVertex {
                position: [-1.0, -1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GpuMesh3dVertex {
                position: [1.0, -1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GpuMesh3dVertex {
                position: [1.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GpuMesh3dVertex {
                position: [-1.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
        ];
        let indices = vec![0, 1, 2, 0, 2, 3];
        let mesh = Arc::new(GpuMesh3d::new(
            vertices,
            indices,
            GpuMesh3dDrawRanges {
                opaque: GpuMesh3dRange { start: 0, count: 6 },
                ..Default::default()
            },
            [0.0, 0.0, 0.0],
            1.0,
            1.0,
            shader,
        ));

        Ok(Arc::new(Self { mesh }))
    }
}

/// Paint a validated fullscreen shader through GPUI's retained custom-mesh scene path.
///
/// The paint closure only emits one mesh draw and one 64-byte parameter block. It performs no
/// image decode, blur, heap construction, or shader compilation on animation frames.
pub(crate) fn shader_effect_canvas(
    program: Arc<ShaderEffectProgram>,
    params: ShaderParams16,
) -> AnyElement {
    let mesh = program.mesh.clone();
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_gpu_mesh_3d(bounds, mesh.clone(), params.as_draw_parameters());
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}
