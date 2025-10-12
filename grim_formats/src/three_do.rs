use std::convert::TryFrom;
use std::io::{Cursor, Read, Seek, SeekFrom};

use anyhow::{Context, Result, bail};
use byteorder::{LittleEndian, ReadBytesExt};
use serde::Serialize;

const MODL_MAGIC: u32 = 0x4d4f444c; // 'MODL' little-endian stored as 'LDOM'

/// Fully decoded 3DO model data.
#[derive(Debug, Clone, Serialize)]
pub struct Model {
    pub name: Option<String>,
    pub materials: Vec<String>,
    pub geosets: Vec<Geoset>,
    pub nodes: Vec<Node>,
    pub radius: f32,
    pub insert_offset: [f32; 3],
}

impl Model {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(bytes);

        let magic = cursor.read_u32::<LittleEndian>()?;
        if magic != MODL_MAGIC {
            bail!("unexpected 3DO magic {magic:#010x}, expected MODL");
        }

        let num_materials = cursor
            .read_u32::<LittleEndian>()?
            .try_into()
            .context("material count does not fit usize")?;
        let mut materials = Vec::with_capacity(num_materials);
        for _ in 0..num_materials {
            materials.push(read_fixed_string(&mut cursor, 32)?);
        }

        let model_name = read_fixed_string(&mut cursor, 32)?;

        // Unknown pointer or flags, currently unused.
        cursor.read_u32::<LittleEndian>()?;

        let num_geosets = cursor
            .read_u32::<LittleEndian>()?
            .try_into()
            .context("geoset count does not fit usize")?;
        let mut geosets = Vec::with_capacity(num_geosets);
        for _ in 0..num_geosets {
            geosets.push(Geoset::read(&mut cursor)?);
        }

        // Skip pointer table.
        cursor.read_u32::<LittleEndian>()?;

        let num_nodes = cursor
            .read_u32::<LittleEndian>()?
            .try_into()
            .context("hierarchy node count does not fit usize")?;
        let mut nodes = Vec::with_capacity(num_nodes);
        for _ in 0..num_nodes {
            nodes.push(Node::read(&mut cursor)?);
        }

        let radius = cursor.read_f32::<LittleEndian>()?;
        skip_bytes(&mut cursor, 36)?;
        let insert_offset = [
            cursor.read_f32::<LittleEndian>()?,
            cursor.read_f32::<LittleEndian>()?,
            cursor.read_f32::<LittleEndian>()?,
        ];

        Ok(Model {
            name: if model_name.is_empty() {
                None
            } else {
                Some(model_name)
            },
            materials,
            geosets,
            nodes,
            radius,
            insert_offset,
        })
    }

    /// Convenience helper to derive triangle lists for every mesh.
    pub fn triangles(&self) -> Vec<Vec<Triangle>> {
        self.geosets
            .iter()
            .flat_map(|geoset| geoset.meshes.iter().map(|mesh| mesh.triangles()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Geoset {
    pub meshes: Vec<Mesh>,
}

impl Geoset {
    fn read(cursor: &mut Cursor<&[u8]>) -> Result<Self> {
        let num_meshes = cursor
            .read_u32::<LittleEndian>()?
            .try_into()
            .context("mesh count does not fit usize")?;
        let mut meshes = Vec::with_capacity(num_meshes);
        for _ in 0..num_meshes {
            meshes.push(Mesh::read(cursor)?);
        }
        Ok(Geoset { meshes })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Mesh {
    pub name: String,
    pub geometry_mode: u32,
    pub lighting_mode: u32,
    pub texture_mode: u32,
    pub shadow: u32,
    pub radius: f32,
    pub vertices: Vec<[f32; 3]>,
    pub vertex_intensity: Vec<f32>,
    pub vertex_normals: Vec<[f32; 3]>,
    pub texture_vertices: Vec<[f32; 2]>,
    pub faces: Vec<Face>,
}

impl Mesh {
    fn read(cursor: &mut Cursor<&[u8]>) -> Result<Self> {
        let name = read_fixed_string(cursor, 32)?;
        // Skip mesh pointer.
        cursor.read_u32::<LittleEndian>()?;
        let geometry_mode = cursor.read_u32::<LittleEndian>()?;
        let lighting_mode = cursor.read_u32::<LittleEndian>()?;
        let texture_mode = cursor.read_u32::<LittleEndian>()?;

        let num_vertices: usize = cursor
            .read_u32::<LittleEndian>()?
            .try_into()
            .context("vertex count does not fit usize")?;
        let num_texture_vertices: usize = cursor
            .read_u32::<LittleEndian>()?
            .try_into()
            .context("texture vertex count does not fit usize")?;
        let num_faces: usize = cursor
            .read_u32::<LittleEndian>()?
            .try_into()
            .context("face count does not fit usize")?;

        let vertices = read_vec3_list(cursor, num_vertices)?;
        let texture_vertices = read_vec2_list(cursor, num_texture_vertices)?;
        let vertex_intensity = read_scalar_list(cursor, num_vertices)?;
        skip_bytes(cursor, bytes_for(num_vertices, 4)?)?;

        let mut faces = Vec::with_capacity(num_faces);
        for _ in 0..num_faces {
            faces.push(Face::read(cursor)?);
        }

        let vertex_normals = read_vec3_list(cursor, num_vertices)?;
        let shadow = cursor.read_u32::<LittleEndian>()?;
        // Skip padding pointer.
        cursor.read_u32::<LittleEndian>()?;
        let radius = cursor.read_f32::<LittleEndian>()?;
        skip_bytes(cursor, 24)?;

        Ok(Mesh {
            name,
            geometry_mode,
            lighting_mode,
            texture_mode,
            shadow,
            radius,
            vertices,
            vertex_intensity,
            vertex_normals,
            texture_vertices,
            faces,
        })
    }

    pub fn triangles(&self) -> Vec<Triangle> {
        let mut tris = Vec::new();
        for (face_index, face) in self.faces.iter().enumerate() {
            if face.vertex_indices.len() < 3 {
                continue;
            }
            // 3DO faces are authored as convex polygons, so treat them as a simple fan.
            for idx in 1..(face.vertex_indices.len() - 1) {
                let tri = Triangle {
                    vertex_indices: [
                        face.vertex_indices[0],
                        face.vertex_indices[idx],
                        face.vertex_indices[idx + 1],
                    ],
                    tex_indices: face
                        .tex_indices
                        .as_ref()
                        .and_then(|indices| Some([indices[0], indices[idx], indices[idx + 1]])),
                    material_index: face.material_index,
                    face_index,
                };
                tris.push(tri);
            }
        }
        tris
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Face {
    pub face_type: u32,
    pub geo: u32,
    pub light: u32,
    pub tex: u32,
    pub extra_light: f32,
    pub vertex_indices: Vec<u32>,
    pub tex_indices: Option<Vec<u32>>,
    pub normal: [f32; 3],
    pub material_index: Option<usize>,
}

impl Face {
    fn read(cursor: &mut Cursor<&[u8]>) -> Result<Self> {
        // Skip face pointer to next.
        cursor.read_u32::<LittleEndian>()?;
        let face_type = cursor.read_u32::<LittleEndian>()?;
        let geo = cursor.read_u32::<LittleEndian>()?;
        let light = cursor.read_u32::<LittleEndian>()?;
        let tex = cursor.read_u32::<LittleEndian>()?;
        let num_vertices: usize = cursor
            .read_u32::<LittleEndian>()?
            .try_into()
            .context("face vertex count does not fit usize")?;

        // Skip surface pointers.
        cursor.read_u32::<LittleEndian>()?;
        let tex_ptr = cursor.read_u32::<LittleEndian>()?;
        let material_ptr_flag = cursor.read_u32::<LittleEndian>()?;
        skip_bytes(cursor, 12)?;
        let extra_light = cursor.read_f32::<LittleEndian>()?;
        skip_bytes(cursor, 12)?;
        let normal = [
            cursor.read_f32::<LittleEndian>()?,
            cursor.read_f32::<LittleEndian>()?,
            cursor.read_f32::<LittleEndian>()?,
        ];

        let vertex_indices = read_u32_list(cursor, num_vertices)?;

        let tex_indices = if tex_ptr != 0 {
            Some(read_u32_list(cursor, num_vertices)?)
        } else {
            None
        };

        let material_index = read_optional_index(cursor, material_ptr_flag)?;

        Ok(Face {
            face_type,
            geo,
            light,
            tex,
            extra_light,
            vertex_indices,
            tex_indices,
            normal,
            material_index,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Triangle {
    pub vertex_indices: [u32; 3],
    pub tex_indices: Option<[u32; 3]>,
    pub material_index: Option<usize>,
    pub face_index: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub name: String,
    pub flags: u32,
    pub node_type: u32,
    pub mesh_index: Option<usize>,
    pub depth: u32,
    pub num_children: u32,
    pub parent: Option<usize>,
    pub child: Option<usize>,
    pub sibling: Option<usize>,
    pub pivot: [f32; 3],
    pub position: [f32; 3],
    pub rotation_yaw_pitch_roll: [f32; 3],
}

impl Node {
    fn read(cursor: &mut Cursor<&[u8]>) -> Result<Self> {
        let name = read_fixed_string(cursor, 64)?;
        let flags = cursor.read_u32::<LittleEndian>()?;
        // Skip pointer to controller data.
        cursor.read_u32::<LittleEndian>()?;
        let node_type = cursor.read_u32::<LittleEndian>()?;
        let mesh_raw = cursor.read_i32::<LittleEndian>()?;
        let mesh_index = if mesh_raw >= 0 {
            Some(usize::try_from(mesh_raw).context("mesh index does not fit usize")?)
        } else {
            None
        };

        let depth = cursor.read_u32::<LittleEndian>()?;
        let parent_flag = cursor.read_u32::<LittleEndian>()?;
        let num_children = cursor.read_u32::<LittleEndian>()?;
        let child_flag = cursor.read_u32::<LittleEndian>()?;
        let sibling_flag = cursor.read_u32::<LittleEndian>()?;

        let pivot = read_vec3(cursor)?;
        let position = read_vec3(cursor)?;
        let pitch = cursor.read_f32::<LittleEndian>()?;
        let yaw = cursor.read_f32::<LittleEndian>()?;
        let roll = cursor.read_f32::<LittleEndian>()?;

        skip_bytes(cursor, 48)?;

        let parent = read_optional_index(cursor, parent_flag)?;
        let child = read_optional_index(cursor, child_flag)?;
        let sibling = read_optional_index(cursor, sibling_flag)?;

        Ok(Node {
            name,
            flags,
            node_type,
            mesh_index,
            depth,
            num_children,
            parent,
            child,
            sibling,
            pivot,
            position,
            rotation_yaw_pitch_roll: [yaw, pitch, roll],
        })
    }
}

fn read_fixed_string(cursor: &mut Cursor<&[u8]>, len: usize) -> Result<String> {
    let mut buf = vec![0u8; len];
    cursor.read_exact(&mut buf)?;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(len);
    let text = std::str::from_utf8(&buf[..end])
        .context("fixed string was not valid UTF-8")?
        .trim_end()
        .to_string();
    Ok(text)
}

fn read_vec3_list(cursor: &mut Cursor<&[u8]>, count: usize) -> Result<Vec<[f32; 3]>> {
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(read_vec3(cursor)?);
    }
    Ok(values)
}

fn read_vec2_list(cursor: &mut Cursor<&[u8]>, count: usize) -> Result<Vec<[f32; 2]>> {
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(read_vec2(cursor)?);
    }
    Ok(values)
}

fn read_vec3(cursor: &mut Cursor<&[u8]>) -> Result<[f32; 3]> {
    Ok([
        cursor.read_f32::<LittleEndian>()?,
        cursor.read_f32::<LittleEndian>()?,
        cursor.read_f32::<LittleEndian>()?,
    ])
}

fn read_vec2(cursor: &mut Cursor<&[u8]>) -> Result<[f32; 2]> {
    Ok([
        cursor.read_f32::<LittleEndian>()?,
        cursor.read_f32::<LittleEndian>()?,
    ])
}

fn read_scalar_list(cursor: &mut Cursor<&[u8]>, count: usize) -> Result<Vec<f32>> {
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(cursor.read_f32::<LittleEndian>()?);
    }
    Ok(values)
}

fn read_u32_list(cursor: &mut Cursor<&[u8]>, count: usize) -> Result<Vec<u32>> {
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(cursor.read_u32::<LittleEndian>()?);
    }
    Ok(values)
}

fn skip_bytes(cursor: &mut Cursor<&[u8]>, count: i64) -> Result<()> {
    cursor
        .seek(SeekFrom::Current(count))
        .context("failed to skip bytes")?;
    Ok(())
}

fn bytes_for(count: usize, size: usize) -> Result<i64> {
    let bytes = count
        .checked_mul(size)
        .context("byte count overflow while skipping")?;
    i64::try_from(bytes).context("byte count does not fit i64")
}

fn read_optional_index(cursor: &mut Cursor<&[u8]>, flag: u32) -> Result<Option<usize>> {
    if flag == 0 {
        return Ok(None);
    }
    let raw = cursor.read_u32::<LittleEndian>()?;
    let index = usize::try_from(raw).context("optional index does not fit usize")?;
    Ok(Some(index))
}
