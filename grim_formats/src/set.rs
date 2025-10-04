use std::str::Lines;

use anyhow::{Result, anyhow};

#[derive(Debug, Clone, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectorKind {
    Walk,
    Camera,
    Special,
    Other,
}

impl SectorKind {
    fn from_str(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "walk" => SectorKind::Walk,
            "camera" => SectorKind::Camera,
            "special" => SectorKind::Special,
            _ => SectorKind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setup {
    pub name: String,
    pub background: Option<String>,
    pub zbuffer: Option<String>,
    pub position: Option<Vec3>,
    pub interest: Option<Vec3>,
    pub roll: Option<f32>,
    pub fov: Option<f32>,
    pub near_clip: Option<f32>,
    pub far_clip: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sector {
    pub name: String,
    pub id: i32,
    pub kind: SectorKind,
    pub default_visibility: Option<String>,
    pub height: f32,
    pub vertices: Vec<Vec3>,
    pub triangles: Vec<[usize; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetFile {
    pub name: Option<String>,
    pub colormaps: Vec<String>,
    pub setups: Vec<Setup>,
    pub sectors: Vec<Sector>,
}

impl SetFile {
    pub fn parse(input: &[u8]) -> Result<Self> {
        let text = String::from_utf8(input.to_vec())?;
        let normalized = text.replace("\r\n", "\n");
        let mut lines = normalized.lines();

        let mut parser = Parser::new(&mut lines);
        parser.parse()
    }
}

struct Parser<'a> {
    lines: &'a mut Lines<'a>,
}

impl<'a> Parser<'a> {
    fn new(lines: &'a mut Lines<'a>) -> Self {
        Self { lines }
    }

    fn parse(&mut self) -> Result<SetFile> {
        let mut colormaps = Vec::new();
        let mut setups = Vec::new();
        let mut sectors = Vec::new();
        let mut section = Section::None;
        let mut current_setup: Option<SetupBuilder> = None;
        let mut current_sector: Option<SectorBuilder> = None;

        while let Some(raw_line) = self.lines.next() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some(value) = line.strip_prefix("section:") {
                section = Section::from_name(value.trim());
                if let Some(builder) = current_setup.take() {
                    setups.push(builder.finish());
                }
                if let Some(builder) = current_sector.take() {
                    sectors.push(builder.finish()?);
                }
                continue;
            }

            match section {
                Section::Colormaps => {
                    if line.starts_with("numcolormaps") {
                        // value unused currently
                    } else if let Some(value) = line.strip_prefix("colormap") {
                        // Format: colormap <name>
                        let parts: Vec<&str> = value.split_whitespace().collect();
                        if let Some(name) = parts.last() {
                            colormaps.push((*name).to_string());
                        }
                    }
                }
                Section::Setups => {
                    if line.starts_with("setup") {
                        if let Some(builder) = current_setup.take() {
                            setups.push(builder.finish());
                        }
                        let name = line
                            .split_whitespace()
                            .last()
                            .ok_or_else(|| anyhow!("missing setup name"))?;
                        current_setup = Some(SetupBuilder::new(name));
                    } else if let Some(builder) = current_setup.as_mut() {
                        builder.consume_line(line)?;
                    }
                }
                Section::Sectors => {
                    if line.starts_with("sector") {
                        if let Some(builder) = current_sector.take() {
                            sectors.push(builder.finish()?);
                        }
                        let name = line
                            .split_whitespace()
                            .last()
                            .ok_or_else(|| anyhow!("missing sector name"))?;
                        current_sector = Some(SectorBuilder::new(name));
                    } else if let Some(builder) = current_sector.as_mut() {
                        builder.consume_line(line, self.lines)?;
                    }
                }
                Section::Other | Section::None => {}
            }
        }

        if let Some(builder) = current_setup.take() {
            setups.push(builder.finish());
        }
        if let Some(builder) = current_sector.take() {
            sectors.push(builder.finish()?);
        }

        Ok(SetFile {
            name: None,
            colormaps,
            setups,
            sectors,
        })
    }
}

#[derive(Debug, Clone)]
struct SetupBuilder {
    name: String,
    background: Option<String>,
    zbuffer: Option<String>,
    position: Option<Vec3>,
    interest: Option<Vec3>,
    roll: Option<f32>,
    fov: Option<f32>,
    near_clip: Option<f32>,
    far_clip: Option<f32>,
}

impl SetupBuilder {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            background: None,
            zbuffer: None,
            position: None,
            interest: None,
            roll: None,
            fov: None,
            near_clip: None,
            far_clip: None,
        }
    }

    fn finish(self) -> Setup {
        Setup {
            name: self.name,
            background: self.background,
            zbuffer: self.zbuffer,
            position: self.position,
            interest: self.interest,
            roll: self.roll,
            fov: self.fov,
            near_clip: self.near_clip,
            far_clip: self.far_clip,
        }
    }

    fn consume_line(&mut self, line: &str) -> Result<()> {
        if line.starts_with("background") {
            if let Some(value) = line.split_whitespace().last() {
                self.background = Some(value.to_string());
            }
        } else if line.starts_with("zbuffer") {
            if let Some(value) = line.split_whitespace().last() {
                self.zbuffer = Some(value.to_string());
            }
        } else if line.starts_with("position") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let tail = parts[1..].join(" ");
                self.position = Some(parse_vec3(&tail)?);
            }
        } else if line.starts_with("interest") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let tail = parts[1..].join(" ");
                self.interest = Some(parse_vec3(&tail)?);
            }
        } else if line.starts_with("roll") {
            if let Some(value) = line.split_whitespace().last() {
                self.roll = Some(value.parse()?);
            }
        } else if line.starts_with("fov") {
            if let Some(value) = line.split_whitespace().last() {
                self.fov = Some(value.parse()?);
            }
        } else if line.starts_with("nclip") {
            if let Some(value) = line.split_whitespace().last() {
                self.near_clip = Some(value.parse()?);
            }
        } else if line.starts_with("fclip") {
            if let Some(value) = line.split_whitespace().last() {
                self.far_clip = Some(value.parse()?);
            }
        }
        Ok(())
    }
}

struct SectorBuilder {
    name: String,
    id: Option<i32>,
    kind: Option<SectorKind>,
    default_visibility: Option<String>,
    height: Option<f32>,
    expected_vertices: Option<usize>,
    expected_tris: Option<usize>,
    vertices: Vec<Vec3>,
    triangles: Vec<[usize; 3]>,
}

impl SectorBuilder {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            id: None,
            kind: None,
            default_visibility: None,
            height: None,
            expected_vertices: None,
            expected_tris: None,
            vertices: Vec::new(),
            triangles: Vec::new(),
        }
    }

    fn finish(self) -> Result<Sector> {
        let id = self
            .id
            .ok_or_else(|| anyhow!("sector '{}' missing ID", self.name))?;
        let kind = self
            .kind
            .ok_or_else(|| anyhow!("sector '{}' missing type", self.name))?;
        let height = self
            .height
            .ok_or_else(|| anyhow!("sector '{}' missing height", self.name))?;
        if let Some(expected) = self.expected_vertices {
            if expected != self.vertices.len() {
                return Err(anyhow!(
                    "sector '{}' expected {} vertices, found {}",
                    self.name,
                    expected,
                    self.vertices.len()
                ));
            }
        }
        if let Some(expected) = self.expected_tris {
            if expected != self.triangles.len() {
                return Err(anyhow!(
                    "sector '{}' expected {} triangles, found {}",
                    self.name,
                    expected,
                    self.triangles.len()
                ));
            }
        }
        Ok(Sector {
            name: self.name,
            id,
            kind,
            default_visibility: self.default_visibility,
            height,
            vertices: self.vertices,
            triangles: self.triangles,
        })
    }

    fn consume_line(&mut self, line: &str, lines: &mut Lines<'_>) -> Result<()> {
        if line.starts_with("ID") {
            let id = line
                .split_whitespace()
                .last()
                .ok_or_else(|| anyhow!("missing sector ID value"))?;
            self.id = Some(id.parse()?);
        } else if line.starts_with("type") {
            if let Some(value) = line.split_whitespace().last() {
                self.kind = Some(SectorKind::from_str(value));
            }
        } else if line.starts_with("default visibility") {
            if let Some(value) = line.split_whitespace().last() {
                self.default_visibility = Some(value.to_string());
            }
        } else if line.starts_with("height") {
            if let Some(value) = line.split_whitespace().last() {
                self.height = Some(value.parse()?);
            }
        } else if line.starts_with("numvertices") {
            if let Some(value) = line.split_whitespace().last() {
                self.expected_vertices = Some(value.parse()?);
            }
        } else if line.starts_with("vertices:") {
            let expected = self
                .expected_vertices
                .ok_or_else(|| anyhow!("numvertices must precede vertices block"))?;
            let target_len = self.vertices.len() + expected;
            let tail = line.splitn(2, ':').nth(1).unwrap_or("").trim();
            if !tail.is_empty() {
                self.vertices.push(parse_vec3(tail)?);
            }
            while self.vertices.len() < target_len {
                let raw = lines
                    .next()
                    .ok_or_else(|| anyhow!("unexpected EOF reading vertices"))?;
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                self.vertices.push(parse_vec3(trimmed)?);
            }
        } else if line.starts_with("numtris") {
            if let Some(value) = line.split_whitespace().last() {
                self.expected_tris = Some(value.parse()?);
            }
        } else if line.starts_with("triangles:") {
            let expected = self
                .expected_tris
                .ok_or_else(|| anyhow!("numtris must precede triangles block"))?;
            let target_len = self.triangles.len() + expected;
            let tail = line.splitn(2, ':').nth(1).unwrap_or("").trim();
            if !tail.is_empty() {
                self.triangles.push(parse_triangle(tail)?);
            }
            while self.triangles.len() < target_len {
                let raw = lines
                    .next()
                    .ok_or_else(|| anyhow!("unexpected EOF reading triangles"))?;
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                self.triangles.push(parse_triangle(trimmed)?);
            }
        }
        Ok(())
    }
}

fn parse_vec3(raw: &str) -> Result<Vec3> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(anyhow!("invalid vertex line: {raw}"));
    }
    let x: f32 = parts[0].parse()?;
    let y: f32 = parts[1].parse()?;
    let z: f32 = parts[2].parse()?;
    Ok(Vec3 { x, y, z })
}

fn parse_triangle(raw: &str) -> Result<[usize; 3]> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(anyhow!("invalid triangle line: {raw}"));
    }
    Ok([parts[0].parse()?, parts[1].parse()?, parts[2].parse()?])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Colormaps,
    Setups,
    Sectors,
    Other,
}

impl Section {
    fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "colormaps" => Section::Colormaps,
            "setups" => Section::Setups,
            "sectors" => Section::Sectors,
            _ => Section::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "section: colormaps\n\tnumcolormaps\t1\n\tcolormap\tprimary.cmp\n\nsection: setups\n\tnumsetups\t1\n\tsetup\tcam_a\n\tposition\t0.0\t0.0\t0.5\n\tinterest\t0.0\t1.0\t0.5\n\troll\t\t0.0\n\tfov\t\t45.0\n\tnclip\t\t0.1\n\tfclip\t\t100.0\n\nsection: sectors\n\tsector\t\tfoo\n\tID\t\t1\n\ttype\t\twalk\n\tdefault visibility\t\tvisible\n\theight\t\t0.50\n\tnumvertices\t3\n\tvertices:\t\t0.0\t0.0\t0.0\n\t         \t\t1.0\t0.0\t0.0\n\t         \t\t0.0\t1.0\t0.0\n\tnumtris 1\n\ttriangles:\t\t0 1 2\n";

    #[test]
    fn parses_minimal_sector() {
        let set = SetFile::parse(SAMPLE.as_bytes()).expect("parse");
        assert_eq!(set.setups.len(), 1);
        let setup = &set.setups[0];
        assert_eq!(setup.name, "cam_a");
        assert!(setup.position.is_some());
        assert!(setup.interest.is_some());
        assert_eq!(set.sectors.len(), 1);
        let sector = &set.sectors[0];
        assert_eq!(sector.name, "foo");
        assert_eq!(sector.id, 1);
        assert_eq!(sector.kind, SectorKind::Walk);
        assert!((sector.height - 0.5).abs() < f32::EPSILON);
        assert_eq!(sector.vertices.len(), 3);
        assert_eq!(sector.triangles.len(), 1);
        assert_eq!(sector.triangles[0], [0, 1, 2]);
    }
}
