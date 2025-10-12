//! Procedural primitive meshes used as stand-ins before decoded assets land.
//! The geometry lives in local space scaled to a unit-ish cube so callers can
//! apply a single uniform scale derived from scene bounds.

use std::f32::consts::PI;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use glam::{EulerRot, Mat3, Mat4, Quat, Vec3};
use serde::Deserialize;

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

/// Flattened vertex/index buffers decoded from a 3DO export.
pub struct AssetMesh {
    pub name: Option<String>,
    pub primitive: MeshPrimitive,
    pub triangle_count: usize,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub radius: Option<f32>,
    pub insert_offset: Option<[f32; 3]>,
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

/// Load a JSON mesh exported via `grim_formats::three_do_export` into flattened buffers.
pub fn load_exported_mesh(path: &Path) -> Result<AssetMesh> {
    let data = fs::read(path).with_context(|| format!("reading mesh JSON {}", path.display()))?;
    let export: ExportModel = serde_json::from_slice(&data)
        .with_context(|| format!("parsing mesh JSON {}", path.display()))?;
    let model_name = export.name.clone();

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut bounds_min = [f32::INFINITY; 3];
    let mut bounds_max = [f32::NEG_INFINITY; 3];
    let mut triangle_count = 0usize;
    let mesh_transforms = compute_mesh_transforms(&export);
    let mut mesh_index = 0usize;

    for geoset in &export.geosets {
        for mesh in &geoset.meshes {
            if mesh.vertices.len() != mesh.vertex_normals.len() {
                bail!(
                    "mesh {} has {} vertices but {} normals",
                    mesh.name,
                    mesh.vertices.len(),
                    mesh.vertex_normals.len()
                );
            }

            let transform = mesh_transforms
                .get(mesh_index)
                .copied()
                .unwrap_or(Mat4::IDENTITY);
            let normal_matrix = Mat3::from_mat4(transform);

            let base_index = vertices.len();
            for (position, normal) in mesh.vertices.iter().zip(mesh.vertex_normals.iter()) {
                let local_position = Vec3::from_array(*position);
                let local_normal = Vec3::from_array(*normal);
                let world = transform.transform_point3(local_position);
                let transformed_normal = (normal_matrix * local_normal).normalize_or_zero();
                vertices.push(MeshVertex {
                    position: world.into(),
                    normal: transformed_normal.into(),
                });
                for axis in 0..3 {
                    bounds_min[axis] = bounds_min[axis].min(world[axis]);
                    bounds_max[axis] = bounds_max[axis].max(world[axis]);
                }
            }

            for tri in &mesh.triangles {
                let mut converted = [0u16; 3];
                for (dst, &raw_index) in converted.iter_mut().zip(tri.vertex_indices.iter()) {
                    let local_index: usize = raw_index
                        .try_into()
                        .context("triangle vertex index does not fit usize")?;
                    let Some(_) = mesh.vertices.get(local_index) else {
                        bail!(
                            "triangle references vertex {} in mesh {} (only {} vertices)",
                            local_index,
                            mesh.name,
                            mesh.vertices.len()
                        );
                    };
                    let global_index = base_index
                        .checked_add(local_index)
                        .ok_or_else(|| anyhow!("global vertex index overflow"))?;
                    if global_index > u16::MAX as usize {
                        bail!(
                            "mesh {} requires index {} which exceeds u16::MAX; split the mesh or upgrade index size",
                            mesh.name,
                            global_index
                        );
                    }
                    *dst = global_index as u16;
                }
                indices.extend_from_slice(&converted);
                triangle_count += 1;
            }
            mesh_index += 1;
        }
    }

    if vertices.is_empty() || triangle_count == 0 {
        let label = model_name
            .clone()
            .unwrap_or_else(|| path.display().to_string());
        bail!("mesh {} contained no geometry", label);
    }

    Ok(AssetMesh {
        name: export.name,
        primitive: MeshPrimitive::new(vertices, indices),
        triangle_count,
        bounds_min,
        bounds_max,
        radius: export.radius,
        insert_offset: export.insert_offset,
    })
}

#[derive(Debug, Deserialize)]
struct ExportModel {
    name: Option<String>,
    geosets: Vec<ExportGeoset>,
    #[serde(default)]
    radius: Option<f32>,
    #[serde(default)]
    insert_offset: Option<[f32; 3]>,
    #[serde(default)]
    nodes: Vec<ExportNode>,
}

#[derive(Debug, Deserialize)]
struct ExportGeoset {
    meshes: Vec<ExportMesh>,
}

#[derive(Debug, Deserialize)]
struct ExportNode {
    #[serde(default)]
    mesh_index: Option<usize>,
    #[serde(default)]
    parent: Option<usize>,
    #[serde(default = "zero_vec3")]
    pivot: [f32; 3],
    #[serde(default = "zero_vec3")]
    position: [f32; 3],
    #[serde(default = "zero_vec3")]
    rotation_yaw_pitch_roll: [f32; 3],
}

#[derive(Debug, Deserialize)]
struct ExportMesh {
    name: String,
    vertices: Vec<[f32; 3]>,
    vertex_normals: Vec<[f32; 3]>,
    #[serde(default)]
    triangles: Vec<ExportTriangle>,
}

#[derive(Debug, Deserialize)]
struct ExportTriangle {
    vertex_indices: [u32; 3],
}

fn zero_vec3() -> [f32; 3] {
    [0.0, 0.0, 0.0]
}

fn compute_mesh_transforms(model: &ExportModel) -> Vec<Mat4> {
    let total_meshes: usize = model.geosets.iter().map(|geoset| geoset.meshes.len()).sum();
    if model.nodes.is_empty() || total_meshes == 0 {
        return vec![Mat4::IDENTITY; total_meshes];
    }
    let node_transforms = compute_node_world_transforms(&model.nodes);
    let mut mesh_transforms = vec![Mat4::IDENTITY; total_meshes];
    for (idx, node) in model.nodes.iter().enumerate() {
        if let Some(mesh_index) = node.mesh_index {
            if mesh_index < mesh_transforms.len() {
                mesh_transforms[mesh_index] = node_transforms[idx];
            }
        }
    }
    mesh_transforms
}

fn compute_node_world_transforms(nodes: &[ExportNode]) -> Vec<Mat4> {
    let mut cache: Vec<Option<Mat4>> = vec![None; nodes.len()];
    for idx in 0..nodes.len() {
        resolve_node_transform(idx, nodes, &mut cache);
    }
    cache
        .into_iter()
        .map(|matrix| matrix.unwrap_or(Mat4::IDENTITY))
        .collect()
}

fn resolve_node_transform(
    idx: usize,
    nodes: &[ExportNode],
    cache: &mut [Option<Mat4>],
) -> Mat4 {
    if let Some(transform) = cache[idx] {
        return transform;
    }
    let node = &nodes[idx];
    let local = node_local_transform(node);
    let world = if let Some(parent_idx) = node.parent {
        let parent = resolve_node_transform(parent_idx, nodes, cache);
        parent * local
    } else {
        local
    };
    cache[idx] = Some(world);
    world
}

fn node_local_transform(node: &ExportNode) -> Mat4 {
    let pivot = Vec3::from_array(node.pivot);
    let position = Vec3::from_array(node.position);
    let yaw = node.rotation_yaw_pitch_roll.get(0).copied().unwrap_or(0.0).to_radians();
    let pitch = node.rotation_yaw_pitch_roll.get(1).copied().unwrap_or(0.0).to_radians();
    let roll = node.rotation_yaw_pitch_roll.get(2).copied().unwrap_or(0.0).to_radians();
    let rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll);
    Mat4::from_translation(position)
        * Mat4::from_translation(pivot)
        * Mat4::from_quat(rotation)
        * Mat4::from_translation(-pivot)
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
