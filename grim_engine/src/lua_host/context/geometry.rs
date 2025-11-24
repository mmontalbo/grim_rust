#[derive(Debug, Clone)]
pub(super) struct SetDescriptor {
    pub(super) variable_name: String,
    pub(super) display_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct SetSnapshot {
    pub(super) set_file: String,
    pub(super) variable_name: String,
    pub(super) display_name: Option<String>,
}

/// Placeholder geometry snapshot. The intro path never inspects polygon data,
/// so we avoid parsing the heavy sector meshes and just retain a marker type.
#[derive(Debug, Clone, Default)]
pub(super) struct ParsedSetGeometry;

impl ParsedSetGeometry {}

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
