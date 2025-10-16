use std::collections::HashMap;
use std::path::Path;

use crate::resources::{ResourceGraph, SetMetadata, SetupSlot};
use crate::runtime::{BootRuntimeModel, RuntimeSet, SetHooks};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct StateCatalog<'a> {
    pub data_root: String,
    pub summary: CatalogSummary,
    pub scripts: CatalogScripts<'a>,
    pub actors: Vec<CatalogActor<'a>>,
    pub sets: Vec<CatalogSet<'a>>,
}

#[derive(Debug, Serialize)]
pub struct CatalogSummary {
    pub total_year_scripts: usize,
    pub total_menu_scripts: usize,
    pub total_room_scripts: usize,
    pub total_sets: usize,
    pub total_actors: usize,
}

#[derive(Debug, Serialize)]
pub struct CatalogScripts<'a> {
    pub years: &'a [String],
    pub menus: &'a [String],
    pub rooms: &'a [String],
}

#[derive(Debug, Serialize)]
pub struct CatalogActor<'a> {
    pub variable_name: &'a str,
    pub label: &'a str,
    pub lua_file: &'a str,
}

#[derive(Debug, Serialize)]
pub struct CatalogSet<'a> {
    pub variable_name: &'a str,
    pub set_file: &'a str,
    pub lua_file: Option<&'a str>,
    pub display_name: Option<&'a str>,
    pub setup_slots: Vec<CatalogSetupSlot<'a>>,
    pub hooks: CatalogSetHooks,
}

#[derive(Debug, Serialize)]
pub struct CatalogSetupSlot<'a> {
    pub label: &'a str,
    pub index: i64,
}

#[derive(Debug, Serialize)]
pub struct CatalogSetHooks {
    pub enter: Option<CatalogFunction>,
    pub exit: Option<CatalogFunction>,
    pub camera_change: Option<CatalogFunction>,
    pub setup: Vec<CatalogFunction>,
    pub other: Vec<CatalogFunction>,
}

#[derive(Debug, Serialize)]
pub struct CatalogFunction {
    pub name: String,
    pub defined_in: String,
    pub defined_at_line: Option<usize>,
    pub parameters: Vec<String>,
}

pub fn build_state_catalog<'a>(
    data_root: &Path,
    resources: &'a ResourceGraph,
    runtime_model: &'a BootRuntimeModel,
) -> StateCatalog<'a> {
    let summary = CatalogSummary {
        total_year_scripts: resources.year_scripts.len(),
        total_menu_scripts: resources.menu_scripts.len(),
        total_room_scripts: resources.room_scripts.len(),
        total_sets: resources.sets.len(),
        total_actors: resources.actors.len(),
    };

    let scripts = CatalogScripts {
        years: &resources.year_scripts,
        menus: &resources.menu_scripts,
        rooms: &resources.room_scripts,
    };

    let actors = resources
        .actors
        .iter()
        .map(|actor| CatalogActor {
            variable_name: actor.variable_name.as_str(),
            label: actor.label.as_str(),
            lua_file: actor.lua_file.as_str(),
        })
        .collect();

    let set_lookup: HashMap<&str, &SetMetadata> = resources
        .sets
        .iter()
        .map(|set| (set.variable_name.as_str(), set))
        .collect();

    let sets = runtime_model
        .sets
        .iter()
        .map(|runtime_set| {
            let metadata = set_lookup.get(runtime_set.variable_name.as_str());
            build_catalog_set(runtime_set, metadata.copied())
        })
        .collect();

    StateCatalog {
        data_root: data_root.display().to_string(),
        summary,
        scripts,
        actors,
        sets,
    }
}

fn build_catalog_set<'a>(
    runtime_set: &'a RuntimeSet,
    metadata: Option<&'a SetMetadata>,
) -> CatalogSet<'a> {
    let setup_slots = metadata
        .map(|meta| {
            meta.setup_slots
                .iter()
                .map(CatalogSetupSlot::from)
                .collect()
        })
        .unwrap_or_default();

    CatalogSet {
        variable_name: runtime_set.variable_name.as_str(),
        set_file: runtime_set.set_file.as_str(),
        lua_file: metadata.map(|meta| meta.lua_file.as_str()),
        display_name: runtime_set.display_name.as_deref(),
        setup_slots,
        hooks: CatalogSetHooks::from(&runtime_set.hooks),
    }
}

impl<'a> From<&'a SetupSlot> for CatalogSetupSlot<'a> {
    fn from(slot: &'a SetupSlot) -> Self {
        Self {
            label: slot.label.as_str(),
            index: slot.index,
        }
    }
}

impl From<&SetHooks> for CatalogSetHooks {
    fn from(hooks: &SetHooks) -> Self {
        Self {
            enter: hooks.enter.as_ref().map(CatalogFunction::from),
            exit: hooks.exit.as_ref().map(CatalogFunction::from),
            camera_change: hooks.camera_change.as_ref().map(CatalogFunction::from),
            setup: hooks
                .setup_functions
                .iter()
                .map(CatalogFunction::from)
                .collect(),
            other: hooks
                .other_methods
                .iter()
                .map(CatalogFunction::from)
                .collect(),
        }
    }
}

impl From<&crate::resources::SetFunction> for CatalogFunction {
    fn from(function: &crate::resources::SetFunction) -> Self {
        Self {
            name: function.name.clone(),
            defined_in: function.defined_in.clone(),
            defined_at_line: function.defined_at_line,
            parameters: function.parameters.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{ActorMetadata, ResourceGraph, SetMetadata, SetupSlot};
    use crate::runtime::{BootRuntimeModel, RuntimeSet, SetHooks};

    #[test]
    fn builds_catalog_with_empty_graph() {
        let resources = ResourceGraph::default();
        let runtime_model = BootRuntimeModel { sets: Vec::new() };
        let catalog = build_state_catalog(Path::new("/tmp"), &resources, &runtime_model);

        assert_eq!(catalog.summary.total_sets, 0);
        assert!(catalog.sets.is_empty());
        assert_eq!(catalog.summary.total_actors, 0);
        assert!(catalog.actors.is_empty());
    }

    #[test]
    fn catalog_includes_basic_metadata() {
        let resources = ResourceGraph {
            year_scripts: vec!["year1.lua".into()],
            menu_scripts: vec!["menu.lua".into()],
            room_scripts: vec!["mo.lua".into()],
            sets: vec![SetMetadata {
                lua_file: "mo.lua".into(),
                variable_name: "mo".into(),
                set_file: "mo.set".into(),
                display_name: Some("Manny's Office".into()),
                setup_slots: vec![SetupSlot {
                    label: "desk".into(),
                    index: 1,
                }],
                methods: Vec::new(),
            }],
            actors: vec![ActorMetadata {
                lua_file: "_actors.lua".into(),
                variable_name: "manny".into(),
                label: "Manny Calavera".into(),
            }],
        };

        let runtime_model = BootRuntimeModel {
            sets: vec![RuntimeSet {
                variable_name: "mo".into(),
                set_file: "mo.set".into(),
                display_name: Some("Manny's Office".into()),
                hooks: SetHooks::default(),
            }],
        };

        let catalog = build_state_catalog(Path::new("/workspace"), &resources, &runtime_model);

        assert_eq!(catalog.summary.total_sets, 1);
        assert_eq!(catalog.summary.total_actors, 1);
        assert_eq!(catalog.scripts.years.len(), 1);
        assert_eq!(catalog.actors[0].variable_name, "manny");
        assert_eq!(catalog.sets[0].lua_file, Some("mo.lua"));
        assert_eq!(catalog.sets[0].setup_slots[0].label, "desk");
    }
}
