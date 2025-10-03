use crate::resources::{ResourceGraph, SetFunction, SetMetadata};

#[derive(Debug, Clone, Default)]
pub struct SetHooks {
    pub enter: Option<SetFunction>,
    pub exit: Option<SetFunction>,
    pub camera_change: Option<SetFunction>,
    pub setup_functions: Vec<SetFunction>,
    pub other_methods: Vec<SetFunction>,
}

#[derive(Debug, Clone)]
pub struct RuntimeSet {
    pub variable_name: String,
    pub set_file: String,
    pub display_name: Option<String>,
    pub hooks: SetHooks,
}

#[derive(Debug, Clone)]
pub struct BootRuntimeModel {
    pub sets: Vec<RuntimeSet>,
}

pub fn build_runtime_model(resources: &ResourceGraph) -> BootRuntimeModel {
    let sets = resources
        .sets
        .iter()
        .map(|set| RuntimeSet {
            variable_name: set.variable_name.clone(),
            set_file: set.set_file.clone(),
            display_name: set.display_name.clone(),
            hooks: classify_hooks(set),
        })
        .collect();

    BootRuntimeModel { sets }
}

fn classify_hooks(set: &SetMetadata) -> SetHooks {
    let mut hooks = SetHooks::default();

    for method in &set.methods {
        if method.name.eq_ignore_ascii_case("enter") {
            hooks.enter = Some(method.clone());
        } else if method.name.eq_ignore_ascii_case("exit") {
            hooks.exit = Some(method.clone());
        } else if method.name.eq_ignore_ascii_case("camerachange") {
            hooks.camera_change = Some(method.clone());
        } else if method.name.starts_with("set_up") {
            hooks.setup_functions.push(method.clone());
        } else {
            hooks.other_methods.push(method.clone());
        }
    }

    hooks.setup_functions.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.defined_at_line.cmp(&b.defined_at_line))
    });
    hooks.other_methods.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.defined_at_line.cmp(&b.defined_at_line))
    });

    hooks
}
