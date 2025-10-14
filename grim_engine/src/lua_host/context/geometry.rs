use grim_formats::{SectorKind as SetSectorKind, SetFile as SetFileData, Vec3 as SetVec3};

#[derive(Debug, Clone)]
pub(super) struct SetupInfo {
    pub(super) label: String,
    pub(super) index: i32,
}

#[derive(Debug, Clone)]
pub(super) struct SetDescriptor {
    pub(super) variable_name: String,
    pub(super) display_name: Option<String>,
    pub(super) setups: Vec<SetupInfo>,
}

impl SetDescriptor {
    pub(super) fn setup_index(&self, label: &str) -> Option<i32> {
        self.setups.iter().find_map(|slot| {
            if slot.label.eq_ignore_ascii_case(label) {
                Some(slot.index)
            } else {
                None
            }
        })
    }

    pub(super) fn setup_label_for_index(&self, index: i32) -> Option<&str> {
        self.setups
            .iter()
            .find(|slot| slot.index == index)
            .map(|slot| slot.label.as_str())
    }

    pub(super) fn first_setup(&self) -> Option<&SetupInfo> {
        self.setups.first()
    }
}

#[derive(Debug, Clone)]
pub(super) struct SetSnapshot {
    pub(super) set_file: String,
    pub(super) variable_name: String,
    pub(super) display_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct SectorPolygon {
    pub(super) name: String,
    pub(super) id: i32,
    pub(super) kind: SetSectorKind,
    pub(super) vertices: Vec<(f32, f32)>,
    pub(super) centroid: (f32, f32),
    pub(super) default_active: bool,
}

impl SectorPolygon {
    fn new(
        name: String,
        id: i32,
        kind: SetSectorKind,
        vertices: Vec<(f32, f32)>,
        default_active: bool,
    ) -> Self {
        let centroid = if vertices.is_empty() {
            (0.0, 0.0)
        } else {
            let (sum_x, sum_y) = vertices
                .iter()
                .fold((0.0, 0.0), |acc, (x, y)| (acc.0 + x, acc.1 + y));
            let count = vertices.len() as f32;
            (sum_x / count, sum_y / count)
        };
        Self {
            name,
            id,
            kind,
            vertices,
            centroid,
            default_active,
        }
    }

    pub(super) fn contains(&self, point: (f32, f32)) -> bool {
        if self.vertices.len() < 3 {
            return false;
        }
        if point_on_polygon_edge(point, &self.vertices) {
            return true;
        }
        ray_cast_contains(point, &self.vertices)
    }

    fn distance_squared(&self, point: (f32, f32)) -> f32 {
        let dx = point.0 - self.centroid.0;
        let dy = point.1 - self.centroid.1;
        dx * dx + dy * dy
    }
}

#[derive(Debug, Clone)]
pub(super) struct ParsedSetup {
    pub(super) name: String,
    pub(super) interest: Option<(f32, f32)>,
    pub(super) position: Option<(f32, f32)>,
}

impl ParsedSetup {
    fn target_point(&self) -> Option<(f32, f32)> {
        self.interest.or(self.position)
    }
}

#[derive(Debug, Clone)]
pub(super) struct ParsedSetGeometry {
    pub(super) sectors: Vec<SectorPolygon>,
    pub(super) setups: Vec<ParsedSetup>,
}

impl ParsedSetGeometry {
    pub(super) fn from_set_file(file: SetFileData) -> Self {
        let sectors = file
            .sectors
            .into_iter()
            .map(|sector| {
                let vertices = sector
                    .vertices
                    .into_iter()
                    .map(|SetVec3 { x, y, .. }| (x, y))
                    .collect();
                let default_active = sector
                    .default_visibility
                    .as_ref()
                    .map(|value| match value.to_ascii_lowercase().as_str() {
                        "hidden" | "invisible" | "false" | "off" => false,
                        _ => true,
                    })
                    .unwrap_or(true);
                SectorPolygon::new(
                    sector.name,
                    sector.id,
                    sector.kind,
                    vertices,
                    default_active,
                )
            })
            .collect();

        let setups = file
            .setups
            .into_iter()
            .map(|setup| ParsedSetup {
                name: setup.name,
                interest: setup.interest.map(|SetVec3 { x, y, .. }| (x, y)),
                position: setup.position.map(|SetVec3 { x, y, .. }| (x, y)),
            })
            .collect();

        ParsedSetGeometry { sectors, setups }
    }

    pub(super) fn has_geometry(&self) -> bool {
        !self.sectors.is_empty() || !self.setups.is_empty()
    }

    pub(super) fn find_polygon(
        &self,
        kind: SetSectorKind,
        point: (f32, f32),
    ) -> Option<&SectorPolygon> {
        let mut fallback = None;
        let mut fallback_dist = f32::MAX;
        for sector in self.sectors.iter().filter(|sector| sector.kind == kind) {
            if sector.contains(point) {
                return Some(sector);
            }
            let dist = sector.distance_squared(point);
            if dist < fallback_dist {
                fallback_dist = dist;
                fallback = Some(sector);
            }
        }
        fallback
    }

    pub(super) fn best_setup_for_point(&self, point: (f32, f32)) -> Option<&ParsedSetup> {
        let mut best = None;
        let mut best_dist = f32::MAX;
        for setup in &self.setups {
            if let Some(target) = setup.target_point() {
                let dx = point.0 - target.0;
                let dy = point.1 - target.1;
                let dist = dx * dx + dy * dy;
                if dist < best_dist {
                    best_dist = dist;
                    best = Some(setup);
                }
            }
        }
        best.or_else(|| self.setups.first())
    }
}

fn point_on_polygon_edge(point: (f32, f32), vertices: &[(f32, f32)]) -> bool {
    if vertices.len() < 2 {
        return false;
    }
    let mut prev = vertices.last().copied().unwrap();
    for &current in vertices {
        if point_on_segment(point, prev, current) {
            return true;
        }
        prev = current;
    }
    false
}

fn point_on_segment(point: (f32, f32), a: (f32, f32), b: (f32, f32)) -> bool {
    let (px, py) = point;
    let (ax, ay) = a;
    let (bx, by) = b;
    let cross = (py - ay) * (bx - ax) - (px - ax) * (by - ay);
    if cross.abs() > 1e-4 {
        return false;
    }
    let dot = (px - ax) * (px - bx) + (py - ay) * (py - by);
    dot <= 0.0
}

fn ray_cast_contains(point: (f32, f32), vertices: &[(f32, f32)]) -> bool {
    let (px, py) = point;
    let mut inside = false;
    let mut j = vertices.len() - 1;
    for i in 0..vertices.len() {
        let (xi, yi) = vertices[i];
        let (xj, yj) = vertices[j];
        if (yi > py) != (yj > py) {
            let denom = yj - yi;
            if denom.abs() > 1e-6 {
                let xinters = (py - yi) * (xj - xi) / denom + xi;
                if xinters > px {
                    inside = !inside;
                }
            }
        }
        j = i;
    }
    inside
}

#[derive(Debug, Clone)]
pub(super) struct SectorHit {
    pub(super) id: i32,
    pub(super) name: String,
    pub(super) kind: String,
}

impl SectorHit {
    pub(super) fn new(id: i32, name: impl Into<String>, kind: impl Into<String>) -> Self {
        SectorHit {
            id,
            name: name.into(),
            kind: kind.into(),
        }
    }
}

pub(super) fn sector_kind_label(kind: SetSectorKind) -> &'static str {
    match kind {
        SetSectorKind::Walk => "walk",
        SetSectorKind::Camera => "camera",
        SetSectorKind::Special => "special",
        SetSectorKind::Other => "other",
    }
}
