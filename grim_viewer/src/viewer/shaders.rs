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
    @location(3) icon: f32,
};

struct VertexIn {
    @location(0) base_pos: vec2<f32>,
    @location(1) translate: vec2<f32>,
    @location(2) depth: f32,
    @location(3) size: f32,
    @location(4) highlight: f32,
    @location(5) color: vec3<f32>,
    @location(6) icon: f32,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOutput {
    let scale = input.size * (1.0 + input.highlight * 0.6);
    let position = input.base_pos * scale + input.translate;
    var out: VertexOutput;
    out.position = vec4<f32>(position, input.depth, 1.0);
    out.color = input.color;
    out.local_pos = input.base_pos;
    out.highlight = input.highlight;
    out.icon = input.icon;
    return out;
}

struct ShapeSample {
    fill: f32,
    rim: f32,
    accent: f32,
};

fn make_sample(fill: f32, rim: f32, accent: f32) -> ShapeSample {
    return ShapeSample(fill, rim, accent);
}

fn sample_circle(local: vec2<f32>) -> ShapeSample {
    let dist = length(local);
    let fill = 1.0 - smoothstep(0.72, 0.78, dist);
    let rim = smoothstep(0.62, 0.72, dist) * (1.0 - smoothstep(0.72, 0.82, dist));
    let accent = 1.0 - smoothstep(0.2, 0.26, dist);
    return make_sample(fill, rim, accent);
}

fn sample_square(local: vec2<f32>) -> ShapeSample {
    let dist = max(abs(local.x), abs(local.y));
    let fill = 1.0 - smoothstep(0.82, 0.88, dist);
    let rim = smoothstep(0.74, 0.82, dist) * (1.0 - smoothstep(0.82, 0.9, dist));
    let accent = 1.0 - smoothstep(0.34, 0.4, dist);
    return make_sample(fill, rim, accent);
}

fn sample_diamond(local: vec2<f32>) -> ShapeSample {
    let dist = (abs(local.x) + abs(local.y)) * 0.70710677;
    let fill = 1.0 - smoothstep(0.78, 0.84, dist);
    let rim = smoothstep(0.7, 0.78, dist) * (1.0 - smoothstep(0.78, 0.86, dist));
    let accent = 1.0 - smoothstep(0.28, 0.34, dist);
    return make_sample(fill, rim, accent);
}

fn sample_ring(local: vec2<f32>) -> ShapeSample {
    let dist = abs(length(local) - 0.54);
    let fill = 1.0 - smoothstep(0.08, 0.12, dist);
    let rim = smoothstep(0.05, 0.08, dist) * (1.0 - smoothstep(0.08, 0.12, dist));
    let accent = 1.0 - smoothstep(0.18, 0.24, length(local));
    return make_sample(fill, rim, accent * 0.35);
}

fn sample_star(local: vec2<f32>) -> ShapeSample {
    let r = vec2<f32>(
        local.x * 0.70710677 - local.y * 0.70710677,
        local.x * 0.70710677 + local.y * 0.70710677,
    );
    let d1 = max(abs(local.x), abs(local.y));
    let d2 = max(abs(r.x), abs(r.y));
    let dist = min(d1, d2);
    let fill = 1.0 - smoothstep(0.8, 0.86, dist);
    let rim = smoothstep(0.72, 0.8, dist) * (1.0 - smoothstep(0.8, 0.88, dist));
    let accent = 1.0 - smoothstep(0.36, 0.44, dist);
    return make_sample(fill, rim, accent);
}

fn sample_panel(local: vec2<f32>) -> ShapeSample {
    let dist = max(abs(local.x), abs(local.y));
    let fill = 1.0 - smoothstep(1.0, 1.04, dist);
    let rim = smoothstep(0.92, 0.98, dist) * (1.0 - smoothstep(0.98, 1.04, dist));
    return make_sample(fill, rim, 0.0);
}

fn sample_path(local: vec2<f32>) -> ShapeSample {
    let scaled = vec2<f32>(local.x, local.y * 1.25);
    let q = vec2<f32>(abs(scaled.x), abs(scaled.y));
    let k = q - vec2<f32>(0.18, 0.48);
    let outside = length(max(k, vec2<f32>(0.0, 0.0))) + min(max(k.x, k.y), 0.0);
    let fill = 1.0 - smoothstep(0.02, 0.08, outside);
    let rim = smoothstep(-0.05, 0.02, outside) * (1.0 - smoothstep(0.02, 0.08, outside));
    let accent = smoothstep(-0.12, -0.02, outside);
    return make_sample(fill, rim, accent * 0.6);
}

fn sample_accent(local: vec2<f32>) -> ShapeSample {
    let bar_x = 1.0 - smoothstep(0.18, 0.24, abs(local.x));
    let bar_y = 1.0 - smoothstep(0.18, 0.24, abs(local.y));
    let cap_x = 1.0 - smoothstep(0.74, 0.8, abs(local.y));
    let cap_y = 1.0 - smoothstep(0.74, 0.8, abs(local.x));
    let vertical = bar_x * cap_x;
    let horizontal = bar_y * cap_y;
    let fill = clamp(vertical + horizontal, 0.0, 1.0);
    let rim = smoothstep(0.64, 0.74, abs(local.x)) * bar_x
        + smoothstep(0.64, 0.74, abs(local.y)) * bar_y;
    let accent = smoothstep(0.0, 0.32, fill) * 0.6;
    return make_sample(clamp(fill, 0.0, 1.0), rim * 0.5, accent);
}

fn sample_icon(icon: u32, local: vec2<f32>) -> ShapeSample {
    switch icon {
        case 0u: {
            return sample_circle(local);
        }
        case 1u: {
            return sample_diamond(local);
        }
        case 2u: {
            return sample_square(local);
        }
        case 3u: {
            return sample_ring(local);
        }
        case 4u: {
            return sample_star(local);
        }
        case 5u: {
            return sample_panel(local);
        }
        case 6u: {
            return sample_path(local);
        }
        default: {
            return sample_accent(local);
        }
    }
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let icon_index = u32(clamp(round(input.icon), 0.0, 7.0));
    let local = input.local_pos * 2.0;
    let shape = sample_icon(icon_index, local);
    let normal = normalize(vec3<f32>(local * vec2<f32>(0.9, 0.8), 1.0));
    let light_dir = normalize(vec3<f32>(-0.45, 0.75, 1.6));
    var light = dot(normal, light_dir);
    light = clamp(light * 0.6 + 0.5, 0.0, 1.0);
    let glow = input.highlight;
    let base = mix(input.color * 0.55, input.color * 1.25, light);
    let glow_color = mix(base, vec3<f32>(1.0, 1.0, 0.92), glow * 0.4);
    let rim_color = mix(vec3<f32>(0.08, 0.1, 0.14), glow_color, glow * 0.55 + 0.2);
    let accent_color = mix(vec3<f32>(1.0, 0.98, 0.82), glow_color, 0.4);
    let color = glow_color * shape.fill
        + rim_color * shape.rim
        + accent_color * shape.accent;
    let alpha = max(shape.fill, max(shape.rim * 0.85, shape.accent * 0.7));
    if alpha < 0.02 {
        discard;
    }
    return vec4<f32>(color, alpha);
}
"#;

pub(super) const MESH_SHADER_SOURCE: &str = r#"
struct SceneUniforms {
    view_projection: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> scene: SceneUniforms;

struct MeshVertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct MeshInstanceIn {
    @location(2) model_col0: vec4<f32>,
    @location(3) model_col1: vec4<f32>,
    @location(4) model_col2: vec4<f32>,
    @location(5) model_col3: vec4<f32>,
    @location(6) color: vec4<f32>,
};

struct MeshVertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn mesh_vs_main(vertex: MeshVertexIn, instance: MeshInstanceIn) -> MeshVertexOut {
    let model = mat4x4<f32>(
        instance.model_col0,
        instance.model_col1,
        instance.model_col2,
        instance.model_col3,
    );
    let world_pos = model * vec4<f32>(vertex.position, 1.0);
    let normal_matrix = mat3x3<f32>(
        instance.model_col0.xyz,
        instance.model_col1.xyz,
        instance.model_col2.xyz,
    );

    var out: MeshVertexOut;
    out.position = scene.view_projection * world_pos;
    out.normal = normalize(normal_matrix * vertex.normal);
    out.color = instance.color;
    return out;
}

@fragment
fn mesh_fs_main(input: MeshVertexOut) -> @location(0) vec4<f32> {
    let normal = normalize(input.normal);
    let light_dir = normalize(vec3<f32>(-0.45, 0.75, 0.52));
    let diffuse = max(dot(normal, light_dir), 0.0);
    let ambient = 0.25;
    let intensity = ambient + diffuse * 0.75;
    let color = input.color.rgb * intensity;
    return vec4<f32>(color, input.color.a);
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
