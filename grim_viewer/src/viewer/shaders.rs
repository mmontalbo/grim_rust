use bytemuck::{Pod, Zeroable};

pub(super) const SHADER_SOURCE: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(input.position, 0.0, 1.0);
    out.uv = input.uv;
    return out;
}

@group(0) @binding(0)
var asset_texture: texture_2d<f32>;
@group(0) @binding(1)
var asset_sampler: sampler;

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let uv = clamp(input.uv, vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0));
    return textureSample(asset_texture, asset_sampler, uv);
}
"#;

pub(super) const MARKER_SHADER_SOURCE: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) local_pos: vec2<f32>,
    @location(2) highlight: f32,
};

struct VertexIn {
    @location(0) base_pos: vec2<f32>,
    @location(1) translate: vec2<f32>,
    @location(2) size: f32,
    @location(3) highlight: f32,
    @location(4) color: vec3<f32>,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOutput {
    let scale = input.size * (1.0 + input.highlight * 0.6);
    let position = input.base_pos * scale + input.translate;
    var out: VertexOutput;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.color = input.color;
    out.local_pos = input.base_pos;
    out.highlight = input.highlight;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let radius = length(input.local_pos) * 1.41421356;
    let inner = 1.0 - smoothstep(0.68, 1.0, radius);
    let border_band = smoothstep(0.62, 0.94, radius) * (1.0 - smoothstep(0.94, 1.08, radius));
    let glow = input.highlight;
    let base_color = mix(input.color, vec3<f32>(1.0, 1.0, 1.0), glow * 0.35);
    let rim_color = mix(vec3<f32>(0.18, 0.2, 0.23), vec3<f32>(1.0, 1.0, 0.85), glow);
    let color = base_color * inner + rim_color * border_band;
    let alpha = max(inner, border_band * 0.9);
    if alpha < 0.03 {
        discard;
    }
    return vec4<f32>(color, alpha);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct QuadVertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
}

pub(super) const QUAD_VERTICES: [QuadVertex; 4] = [
    QuadVertex {
        position: [-1.0, 1.0],
        uv: [0.0, 0.0],
    },
    QuadVertex {
        position: [1.0, 1.0],
        uv: [1.0, 0.0],
    },
    QuadVertex {
        position: [-1.0, -1.0],
        uv: [0.0, 1.0],
    },
    QuadVertex {
        position: [1.0, -1.0],
        uv: [1.0, 1.0],
    },
];

pub(super) const QUAD_INDICES: [u16; 6] = [0, 1, 2, 2, 1, 3];
