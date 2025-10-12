//! Convert LucasArts 3DO meshes into a JSON description that the viewer can load.
//! The schema mirrors the decoded `three_do` structs (materials, meshes, faces, triangles).

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use grim_formats::{
    ThreeDoFace, ThreeDoGeoset, ThreeDoMesh, ThreeDoModel, ThreeDoNode, ThreeDoTriangle,
};
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Input 3DO file to convert
    #[arg(long)]
    input: PathBuf,

    /// Output JSON file path
    #[arg(long)]
    output: PathBuf,

    /// Pretty-print the JSON output
    #[arg(long, default_value_t = false)]
    pretty: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let bytes = fs::read(&args.input)?;
    let model = ThreeDoModel::from_bytes(&bytes)?;
    let export = ExportModel::from(&model);

    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let file = File::create(&args.output)?;
    let mut writer = BufWriter::new(file);
    if args.pretty {
        serde_json::to_writer_pretty(&mut writer, &export)?;
    } else {
        serde_json::to_writer(&mut writer, &export)?;
    }
    writer.flush()?;

    Ok(())
}

#[derive(Debug, Serialize)]
struct ExportModel {
    name: Option<String>,
    materials: Vec<String>,
    radius: f32,
    insert_offset: [f32; 3],
    geosets: Vec<ExportGeoset>,
    nodes: Vec<ThreeDoNode>,
}

impl From<&ThreeDoModel> for ExportModel {
    fn from(model: &ThreeDoModel) -> Self {
        ExportModel {
            name: model.name.clone(),
            materials: model.materials.clone(),
            radius: model.radius,
            insert_offset: model.insert_offset,
            geosets: model.geosets.iter().map(ExportGeoset::from).collect(),
            nodes: model.nodes.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ExportGeoset {
    meshes: Vec<ExportMesh>,
}

impl From<&ThreeDoGeoset> for ExportGeoset {
    fn from(geoset: &ThreeDoGeoset) -> Self {
        ExportGeoset {
            meshes: geoset.meshes.iter().map(ExportMesh::from).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ExportMesh {
    name: String,
    geometry_mode: u32,
    lighting_mode: u32,
    texture_mode: u32,
    shadow: u32,
    radius: f32,
    vertices: Vec<[f32; 3]>,
    vertex_intensity: Vec<f32>,
    vertex_normals: Vec<[f32; 3]>,
    texture_vertices: Vec<[f32; 2]>,
    faces: Vec<ThreeDoFace>,
    triangles: Vec<ThreeDoTriangle>,
}

impl From<&ThreeDoMesh> for ExportMesh {
    fn from(mesh: &ThreeDoMesh) -> Self {
        ExportMesh {
            name: mesh.name.clone(),
            geometry_mode: mesh.geometry_mode,
            lighting_mode: mesh.lighting_mode,
            texture_mode: mesh.texture_mode,
            shadow: mesh.shadow,
            radius: mesh.radius,
            vertices: mesh.vertices.clone(),
            vertex_intensity: mesh.vertex_intensity.clone(),
            vertex_normals: mesh.vertex_normals.clone(),
            texture_vertices: mesh.texture_vertices.clone(),
            faces: mesh.faces.clone(),
            triangles: mesh.triangles(),
        }
    }
}
