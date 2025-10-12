//! Procedural primitive meshes used as stand-ins before decoded assets land.
//! The geometry lives in local space scaled to a unit-ish cube so callers can
//! apply a single uniform scale derived from scene bounds.

use std::f32::consts::PI;

use glam::{Mat4, Quat, Vec3};

use crate::scene::SceneBounds;

use bytemuck::{Pod, Zeroable};

const DEFAULT_SPHERE_LAT_DIVS: u32 = 12;
const DEFAULT_SPHERE_LON_DIVS: u32 = 18;
const DEFAULT_CONE_SEGMENTS: u32 = 16;

const BASE_SCALE_MIN: f32 = 0.05;
const BASE_SCALE_MAX: f32 = 2.5;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

pub struct MeshPrimitive {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u16>,
}

impl MeshPrimitive {
    pub fn new(vertices: Vec<MeshVertex>, indices: Vec<u16>) -> Self {
        Self { vertices, indices }
    }
}

#[derive(Clone, Copy)]
pub enum PrimitiveKind {
    Sphere,
    Cube,
    Cone,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct MeshInstance {
    pub model: [[f32; 4]; 4],
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct MeshUniforms {
    pub view_projection: [[f32; 4]; 4],
}

pub fn primitive(kind: PrimitiveKind) -> MeshPrimitive {
    match kind {
        PrimitiveKind::Sphere => build_sphere(DEFAULT_SPHERE_LAT_DIVS, DEFAULT_SPHERE_LON_DIVS),
        PrimitiveKind::Cube => build_cube(),
        PrimitiveKind::Cone => build_cone(DEFAULT_CONE_SEGMENTS),
    }
}

pub fn instance_transform(position: [f32; 3], scale: f32) -> [[f32; 4]; 4] {
    instance_transform_oriented(position, scale, Quat::IDENTITY)
}

pub fn instance_transform_oriented(
    position: [f32; 3],
    scale: f32,
    rotation: Quat,
) -> [[f32; 4]; 4] {
    let translation = Vec3::from(position);
    let transform =
        Mat4::from_scale_rotation_translation(Vec3::splat(scale), rotation, translation);
    to_matrix_columns(transform)
}

pub fn view_projection_uniform(matrix: Mat4) -> MeshUniforms {
    MeshUniforms {
        view_projection: to_matrix_columns(matrix),
    }
}

/// Derive a nominal scale in world units by looking at the scene bounds span.
pub fn bounds_scale(bounds: Option<&SceneBounds>) -> f32 {
    bounds
        .map(|b| {
            let span_x = (b.max[0] - b.min[0]).abs();
            let span_y = (b.max[1] - b.min[1]).abs();
            let span_z = (b.max[2] - b.min[2]).abs();
            let span = span_x.max(span_y).max(span_z);
            (span * 0.04).clamp(BASE_SCALE_MIN, BASE_SCALE_MAX)
        })
        .unwrap_or(0.35)
}

fn to_matrix_columns(matrix: Mat4) -> [[f32; 4]; 4] {
    let data = matrix.to_cols_array();
    [
        [data[0], data[1], data[2], data[3]],
        [data[4], data[5], data[6], data[7]],
        [data[8], data[9], data[10], data[11]],
        [data[12], data[13], data[14], data[15]],
    ]
}

fn build_sphere(lat_divisions: u32, lon_divisions: u32) -> MeshPrimitive {
    let lat_steps = lat_divisions.max(3);
    let lon_steps = lon_divisions.max(6);
    let mut vertices = Vec::with_capacity(((lat_steps + 1) * (lon_steps + 1)) as usize);
    let mut indices = Vec::with_capacity((lat_steps * lon_steps * 6) as usize);

    for lat in 0..=lat_steps {
        let v = lat as f32 / lat_steps as f32;
        let theta = v * PI;
        let sin_theta = theta.sin();
        let cos_theta = theta.cos();

        for lon in 0..=lon_steps {
            let u = lon as f32 / lon_steps as f32;
            let phi = u * PI * 2.0;
            let sin_phi = phi.sin();
            let cos_phi = phi.cos();

            let x = sin_theta * cos_phi;
            let y = cos_theta;
            let z = sin_theta * sin_phi;
            let normal = Vec3::new(x, y, z).normalize();
            vertices.push(MeshVertex {
                position: [x * 0.5, y * 0.5, z * 0.5],
                normal: normal.into(),
            });
        }
    }

    let ring = (lon_steps + 1) as usize;
    for lat in 0..lat_steps as usize {
        for lon in 0..lon_steps as usize {
            let current = lat * ring + lon;
            let next = current + ring;
            indices.push(current as u16);
            indices.push(next as u16);
            indices.push((current + 1) as u16);

            indices.push((current + 1) as u16);
            indices.push(next as u16);
            indices.push((next + 1) as u16);
        }
    }

    MeshPrimitive::new(vertices, indices)
}

fn build_cube() -> MeshPrimitive {
    #[rustfmt::skip]
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        // +X
        (
            [1.0, 0.0, 0.0],
            [
                [0.5, -0.5, -0.5],
                [0.5, 0.5, -0.5],
                [0.5, 0.5, 0.5],
                [0.5, -0.5, 0.5],
            ],
        ),
        // -X
        (
            [-1.0, 0.0, 0.0],
            [
                [-0.5, -0.5, 0.5],
                [-0.5, 0.5, 0.5],
                [-0.5, 0.5, -0.5],
                [-0.5, -0.5, -0.5],
            ],
        ),
        // +Y
        (
            [0.0, 1.0, 0.0],
            [
                [-0.5, 0.5, -0.5],
                [-0.5, 0.5, 0.5],
                [0.5, 0.5, 0.5],
                [0.5, 0.5, -0.5],
            ],
        ),
        // -Y
        (
            [0.0, -1.0, 0.0],
            [
                [-0.5, -0.5, 0.5],
                [-0.5, -0.5, -0.5],
                [0.5, -0.5, -0.5],
                [0.5, -0.5, 0.5],
            ],
        ),
        // +Z
        (
            [0.0, 0.0, 1.0],
            [
                [-0.5, -0.5, 0.5],
                [0.5, -0.5, 0.5],
                [0.5, 0.5, 0.5],
                [-0.5, 0.5, 0.5],
            ],
        ),
        // -Z
        (
            [0.0, 0.0, -1.0],
            [
                [0.5, -0.5, -0.5],
                [-0.5, -0.5, -0.5],
                [-0.5, 0.5, -0.5],
                [0.5, 0.5, -0.5],
            ],
        ),
    ];

    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (face_index, (normal, corners)) in faces.iter().enumerate() {
        let base = (face_index * 4) as u16;
        for corner in corners {
            vertices.push(MeshVertex {
                position: *corner,
                normal: *normal,
            });
        }
        indices.push(base);
        indices.push(base + 1);
        indices.push(base + 2);
        indices.push(base);
        indices.push(base + 2);
        indices.push(base + 3);
    }

    MeshPrimitive::new(vertices, indices)
}

fn build_cone(segments: u32) -> MeshPrimitive {
    let ring = segments.max(3);
    let mut vertices = Vec::with_capacity((ring * 2 + 2) as usize);
    let mut indices = Vec::with_capacity((ring * 6) as usize);

    let apex_index = vertices.len() as u16;
    vertices.push(MeshVertex {
        position: [0.0, 0.5, 0.0],
        normal: [0.0, 1.0, 0.0],
    });

    for i in 0..ring {
        let angle = (i as f32 / ring as f32) * PI * 2.0;
        let x = angle.cos() * 0.5;
        let z = angle.sin() * 0.5;
        let normal = Vec3::new(x, 0.35, z).normalize();
        vertices.push(MeshVertex {
            position: [x, -0.5, z],
            normal: normal.into(),
        });
    }

    for i in 0..ring {
        let current = 1 + i as u16;
        let next = 1 + ((i + 1) % ring) as u16;
        indices.push(apex_index);
        indices.push(current);
        indices.push(next);
    }

    let base_center_index = vertices.len() as u16;
    vertices.push(MeshVertex {
        position: [0.0, -0.5, 0.0],
        normal: [0.0, -1.0, 0.0],
    });

    for i in 0..ring {
        let angle = (i as f32 / ring as f32) * PI * 2.0;
        let x = angle.cos() * 0.5;
        let z = angle.sin() * 0.5;
        vertices.push(MeshVertex {
            position: [x, -0.5, z],
            normal: [0.0, -1.0, 0.0],
        });
    }

    for i in 0..ring {
        let current = base_center_index + 1 + i as u16;
        let next = base_center_index + 1 + ((i + 1) % ring) as u16;
        indices.push(base_center_index);
        indices.push(next);
        indices.push(current);
    }

    MeshPrimitive::new(vertices, indices)
}
