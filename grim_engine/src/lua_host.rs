use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context, Result};
use grim_analysis::resources::{normalize_legacy_lua, ResourceGraph};
use mlua::{
    Error as LuaError, Function, Lua, LuaOptions, MultiValue, RegistryKey, Result as LuaResult,
    StdLib, Table, Value, Variadic,
};

#[derive(Debug, Clone)]
struct ScriptRecord {
    label: String,
}

#[derive(Debug, Clone)]
struct SetDescriptor {
    variable_name: String,
    display_name: Option<String>,
}

#[derive(Debug, Clone)]
struct SetSnapshot {
    set_file: String,
    variable_name: String,
    display_name: Option<String>,
}

#[derive(Debug, Copy, Clone)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Debug, Default, Clone)]
struct ActorSnapshot {
    name: String,
    costume: Option<String>,
    current_set: Option<String>,
    at_interest: bool,
    position: Option<Vec3>,
    rotation: Option<Vec3>,
    is_selected: bool,
}

#[derive(Debug)]
struct EngineContext {
    verbose: bool,
    _resources: Rc<ResourceGraph>,
    next_script_handle: u32,
    scripts: BTreeMap<u32, ScriptRecord>,
    events: Vec<String>,
    current_set: Option<SetSnapshot>,
    selected_actor: Option<String>,
    actors: BTreeMap<String, ActorSnapshot>,
    available_sets: BTreeMap<String, SetDescriptor>,
    loaded_sets: BTreeSet<String>,
    inventory: BTreeSet<String>,
}

impl EngineContext {
    fn new(resources: Rc<ResourceGraph>, verbose: bool) -> Self {
        let mut available_sets = BTreeMap::new();
        for meta in &resources.sets {
            available_sets.insert(
                meta.set_file.clone(),
                SetDescriptor {
                    variable_name: meta.variable_name.clone(),
                    display_name: meta.display_name.clone(),
                },
            );
        }

        EngineContext {
            verbose,
            _resources: resources,
            next_script_handle: 1,
            scripts: BTreeMap::new(),
            events: Vec::new(),
            current_set: None,
            selected_actor: None,
            actors: BTreeMap::new(),
            available_sets,
            loaded_sets: BTreeSet::new(),
            inventory: BTreeSet::new(),
        }
    }

    fn log_event(&mut self, event: impl Into<String>) {
        self.events.push(event.into());
    }

    fn start_script(&mut self, label: String) -> u32 {
        let handle = self.next_script_handle;
        self.next_script_handle += 1;
        self.scripts.insert(
            handle,
            ScriptRecord {
                label: label.clone(),
            },
        );
        self.log_event(format!("script.start {label} (#{handle})"));
        handle
    }

    fn has_script_with_label(&self, label: &str) -> bool {
        self.scripts.values().any(|record| record.label == label)
    }

    fn complete_script(&mut self, handle: u32) {
        if let Some(record) = self.scripts.remove(&handle) {
            self.log_event(format!("script.complete {} (#{handle})", record.label));
        }
    }

    fn ensure_actor_mut(&mut self, id: &str, label: &str) -> &mut ActorSnapshot {
        self.actors.entry(id.to_string()).or_insert_with(|| {
            let mut actor = ActorSnapshot::default();
            actor.name = label.to_string();
            actor
        })
    }

    fn select_actor(&mut self, id: &str, label: &str) {
        if let Some(previous) = self.selected_actor.take() {
            if let Some(actor) = self.actors.get_mut(&previous) {
                actor.is_selected = false;
            }
        }
        let actor = self.ensure_actor_mut(id, label);
        actor.is_selected = true;
        self.selected_actor = Some(id.to_string());
        self.log_event(format!("actor.select {id}"));
    }

    fn switch_to_set(&mut self, set_file: &str) {
        let (variable_name, display_name) = match self.available_sets.get(set_file) {
            Some(descriptor) => (
                descriptor.variable_name.clone(),
                descriptor.display_name.clone(),
            ),
            None => (set_file.to_string(), None),
        };
        self.current_set = Some(SetSnapshot {
            set_file: set_file.to_string(),
            variable_name,
            display_name,
        });
        self.log_event(format!("set.switch {set_file}"));
    }

    fn mark_set_loaded(&mut self, set_file: &str) {
        if self.loaded_sets.insert(set_file.to_string()) {
            self.log_event(format!("set.load {set_file}"));
        }
    }

    fn set_actor_costume(&mut self, id: &str, label: &str, costume: Option<String>) {
        let actor = self.ensure_actor_mut(id, label);
        actor.costume = costume.clone();
        if let Some(name) = costume {
            self.log_event(format!("actor.{id}.costume {name}"));
        }
    }

    fn put_actor_in_set(&mut self, id: &str, label: &str, set_file: &str) {
        let actor = self.ensure_actor_mut(id, label);
        actor.current_set = Some(set_file.to_string());
        self.log_event(format!("actor.{id}.enter {set_file}"));
    }

    fn actor_at_interest(&mut self, id: &str, label: &str) {
        let actor = self.ensure_actor_mut(id, label);
        actor.at_interest = true;
        self.log_event(format!("actor.{id}.at_interest"));
    }

    fn set_actor_position(&mut self, id: &str, label: &str, position: Vec3) {
        let actor = self.ensure_actor_mut(id, label);
        actor.position = Some(position);
        self.log_event(format!(
            "actor.{id}.pos {:.3},{:.3},{:.3}",
            position.x, position.y, position.z
        ));
    }

    fn set_actor_rotation(&mut self, id: &str, label: &str, rotation: Vec3) {
        let actor = self.ensure_actor_mut(id, label);
        actor.rotation = Some(rotation);
        self.log_event(format!(
            "actor.{id}.rot {:.3},{:.3},{:.3}",
            rotation.x, rotation.y, rotation.z
        ));
    }

    fn add_inventory_item(&mut self, name: &str) {
        if self.inventory.insert(name.to_string()) {
            self.log_event(format!("inventory.add {name}"));
        }
    }

    fn find_script_handle(&self, label: &str) -> Option<u32> {
        self.scripts
            .iter()
            .find_map(|(handle, record)| (record.label == label).then_some(*handle))
    }
}

pub fn run_boot_sequence(data_root: &Path, verbose: bool) -> Result<()> {
    let resources = Rc::new(
        ResourceGraph::from_data_root(data_root)
            .with_context(|| format!("loading resource graph from {}", data_root.display()))?,
    );

    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
        .context("initialising Lua runtime with standard libraries")?;
    let context = Rc::new(RefCell::new(EngineContext::new(resources, verbose)));

    install_package_path(&lua, data_root)?;
    install_globals(&lua, data_root, context.clone())?;
    load_system_script(&lua, data_root)?;
    override_boot_stubs(&lua, context.clone())?;
    call_boot(&lua, context.clone())?;

    let snapshot = context.borrow();
    dump_runtime_summary(&snapshot);
    Ok(())
}

fn install_package_path(lua: &Lua, data_root: &Path) -> Result<()> {
    let globals = lua.globals();
    let package: Table = globals
        .get("package")
        .context("package table missing from Lua state")?;
    let current_path: String = package.get("path")?;
    let mut paths = vec![format!("{}/?.lua", data_root.display())];
    paths.push(format!("{}/?.decompiled.lua", data_root.display()));
    paths.push(current_path);
    let new_path = paths.join(";");
    package.set("path", new_path)?;
    Ok(())
}

fn install_globals(lua: &Lua, data_root: &Path, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let root = data_root.to_path_buf();
    let verbose_context = context.clone();
    let wrapped_dofile = lua.create_function(move |lua_ctx, path: String| -> LuaResult<Value> {
        if Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                let lower = name.to_ascii_lowercase();
                matches!(
                    lower.as_str(),
                    "setfallback.lua" | "_actors.lua" | "_actors.decompiled.lua"
                )
            })
            .unwrap_or(false)
        {
            if verbose_context.borrow().verbose {
                println!("[lua][dofile] skipping {} via host", path);
            }
            return Ok(Value::Nil);
        }
        let mut tried = Vec::new();
        let candidates = candidate_paths(&path);
        for candidate in candidates {
            let absolute = if candidate.is_absolute() {
                candidate.clone()
            } else {
                root.join(&candidate)
            };
            tried.push(absolute.clone());
            if let Some(value) = execute_script(lua_ctx, &absolute)? {
                if verbose_context.borrow().verbose {
                    println!("[lua][dofile] loaded {}", absolute.display());
                }
                return Ok(value);
            }
        }
        if verbose_context.borrow().verbose {
            println!("[lua][dofile] skipped {}", path);
            for attempt in tried {
                println!("  tried {}", attempt.display());
            }
        }
        Ok(Value::Nil)
    })?;
    globals.set("dofile", wrapped_dofile)?;

    install_logging_functions(lua, context.clone())?;
    install_engine_bindings(lua, context.clone())?;
    install_runtime_tables(lua, context)?;

    Ok(())
}

fn candidate_paths(path: &str) -> Vec<PathBuf> {
    let mut base_candidates = Vec::new();
    base_candidates.push(path.to_string());

    if path.ends_with(".lua") {
        let mut alt = path.to_string();
        alt.truncate(alt.len().saturating_sub(4));
        alt.push_str(".decompiled.lua");
        base_candidates.push(alt);
    } else if path.ends_with(".decompiled.lua") {
        let mut alt = path.to_string();
        alt.truncate(alt.len().saturating_sub(".decompiled.lua".len()));
        alt.push_str(".lua");
        base_candidates.push(alt);
    } else {
        base_candidates.push(format!("{path}.lua"));
        base_candidates.push(format!("{path}.decompiled.lua"));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.iter().any(|existing| existing == &candidate) {
            candidates.push(candidate);
        }
    };

    for candidate in base_candidates {
        let direct = PathBuf::from(&candidate);
        push_unique(direct.clone());
        push_unique(PathBuf::from("Scripts").join(&direct));
    }

    candidates
}

fn execute_script<'lua>(lua: &'lua Lua, path: &Path) -> LuaResult<Option<Value<'lua>>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(LuaError::external)?;
    let chunk_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("script");
    let eval_result = if path.to_string_lossy().ends_with(".decompiled.lua") {
        let source = String::from_utf8_lossy(&bytes);
        let script = normalize_legacy_lua(&source);
        lua.load(&script).set_name(chunk_name).eval::<MultiValue>()
    } else if is_precompiled_chunk(&bytes) {
        lua.load(&bytes).set_name(chunk_name).eval::<MultiValue>()
    } else {
        let source = String::from_utf8_lossy(&bytes).into_owned();
        lua.load(&source).set_name(chunk_name).eval::<MultiValue>()
    };

    match eval_result {
        Ok(results) => Ok(results.into_iter().next()),
        Err(LuaError::SyntaxError { message, .. })
            if message.contains("bad header in precompiled chunk") =>
        {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

fn is_precompiled_chunk(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0] == 0x1B && bytes[1] == b'L' && bytes[2] == b'u' && bytes[3] == b'a'
}

fn install_logging_functions(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let debug_state = context.clone();
    let print_debug = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.get(0) {
            if debug_state.borrow().verbose {
                println!("[lua][PrintDebug] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    globals.set("PrintDebug", print_debug)?;

    let logf_state = context.clone();
    let logf = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.get(0) {
            if logf_state.borrow().verbose {
                println!("[lua][logf] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    globals.set("logf", logf)?;

    Ok(())
}

fn install_engine_bindings(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let noop = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    let nil_return = lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?;

    globals.set(
        "LockFont",
        lua.create_function(|_, name: String| Ok(format!("font::{name}")))?,
    )?;
    globals.set(
        "LockCursor",
        lua.create_function(|_, name: String| Ok(format!("cursor::{name}")))?,
    )?;
    globals.set("SetSayLineDefaults", noop.clone())?;
    globals.set("GetPlatform", lua.create_function(|_, ()| Ok(1))?)?; // PLATFORM_PC_WIN
    globals.set("ReadRegistryValue", nil_return.clone())?;
    globals.set("ReadRegistryIntValue", nil_return.clone())?;
    globals.set("WriteRegistryValue", noop.clone())?;
    globals.set("enable_basic_remappable_key_set", noop.clone())?;
    globals.set("enable_joystick_controls", noop.clone())?;
    globals.set("enable_mouse_controls", noop.clone())?;
    globals.set(
        "AreAchievementsInstalled",
        lua.create_function(|_, ()| Ok(1))?,
    )?;
    globals.set("GlobalSaveResolved", lua.create_function(|_, ()| Ok(1))?)?;
    globals.set(
        "CheckForFile",
        lua.create_function(|_, _args: Variadic<Value>| Ok(true))?,
    )?;
    globals.set(
        "CheckForCD",
        lua.create_function(|_, _args: Variadic<Value>| Ok((false, false)))?,
    )?;
    globals.set("NukeResources", noop.clone())?;
    globals.set("GetSystemFonts", noop.clone())?;
    globals.set("PreloadCursors", noop.clone())?;
    globals.set("break_here", noop.clone())?;
    globals.set("HideVerbSkull", noop.clone())?;
    globals.set("MakeCurrentSet", noop.clone())?;
    globals.set("MakeCurrentSetup", noop.clone())?;
    globals.set("SetAmbientLight", noop.clone())?;
    globals.set("LightMgrSetChange", noop.clone())?;
    globals.set("HideMouseCursor", noop.clone())?;
    globals.set("ShowCursor", noop.clone())?;
    globals.set("SetShadowColor", noop.clone())?;
    globals.set("SetActiveShadow", noop.clone())?;
    globals.set("SetActorShadowPoint", noop.clone())?;
    globals.set("SetActorShadowPlane", noop.clone())?;
    globals.set("AddShadowPlane", noop.clone())?;
    globals.set("LoadCostume", noop.clone())?;
    globals.set(
        "tag",
        lua.create_function(|_, _args: Variadic<Value>| Ok(0))?,
    )?;
    globals.set("settagmethod", noop.clone())?;
    globals.set("setfallback", noop.clone())?;
    globals.set(
        "look_up_correct_costume",
        lua.create_function(|_, _args: Variadic<Value>| Ok(String::from("suit")))?,
    )?;
    globals.set("gettagmethod", nil_return.clone())?;
    globals.set("getglobal", nil_return.clone())?;
    globals.set("setglobal", noop.clone())?;
    globals.set("GlobalShrinkEnabled", false)?;
    globals.set("shrinkBoxesEnabled", false)?;
    globals.set(
        "randomseed",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;
    globals.set("random", lua.create_function(|_, ()| Ok(0.42))?)?;
    globals.set("sleep_for", noop.clone())?;
    globals.set("set_override", noop.clone())?;
    globals.set("kill_override", noop.clone())?;
    globals.set("FadeInChore", noop.clone())?;
    globals.set("START_CUT_SCENE", noop.clone())?;
    globals.set("END_CUT_SCENE", noop.clone())?;
    globals.set("wait_for_message", noop.clone())?;
    globals.set(
        "Load",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;

    let loading_menu = lua.create_table()?;
    let loading_visible = Rc::new(RefCell::new(false));
    let visible_clone = loading_visible.clone();
    let run_fn = lua.create_function(move |_, _args: Variadic<Value>| {
        *visible_clone.borrow_mut() = true;
        Ok(())
    })?;
    loading_menu.set("run", run_fn)?;
    let visible_clone = loading_visible.clone();
    let close_fn = lua.create_function(move |_, ()| {
        *visible_clone.borrow_mut() = false;
        Ok(())
    })?;
    loading_menu.set("close", close_fn)?;
    let visible_clone = loading_visible.clone();
    let is_visible_fn = lua.create_function(move |_, ()| Ok(*visible_clone.borrow()))?;
    loading_menu.set("is_visible", is_visible_fn)?;
    globals.set("loading_menu", loading_menu)?;

    let boot_menu = lua.create_table()?;
    boot_menu.set("run", noop.clone())?;
    boot_menu.set("check_timeout", lua.create_function(|_, ()| Ok(()))?)?;
    globals.set("boot_warning_menu", boot_menu)?;

    let prefs = lua.create_table()?;
    prefs.set("init", noop.clone())?;
    prefs.set("write", noop.clone())?;
    globals.set("system_prefs", prefs)?;

    let concept_menu = lua.create_table()?;
    concept_menu.set("unlock_concepts", noop.clone())?;
    globals.set("concept_menu", concept_menu)?;

    let inventory_state = context.clone();
    let inventory = lua.create_table()?;
    inventory.set("unordered_inventory_table", lua.create_table()?)?;
    inventory.set(
        "add_item_to_inventory",
        lua.create_function(move |_, args: Variadic<Value>| {
            if let Some(Value::Table(item)) = args.get(0) {
                if let Ok(name) = item.get::<_, String>("name") {
                    inventory_state.borrow_mut().add_inventory_item(&name);
                    return Ok(());
                }
            }
            if let Some(Value::String(name)) = args.get(0) {
                inventory_state
                    .borrow_mut()
                    .add_inventory_item(name.to_str()?);
            }
            Ok(())
        })?,
    )?;
    globals.set("Inventory", inventory)?;

    let cut_scene = lua.create_table()?;
    let runtime_clone = context.clone();
    cut_scene.set(
        "logos",
        lua.create_function(move |_, ()| {
            runtime_clone
                .borrow_mut()
                .log_event("cut_scene.logos scheduled");
            Ok(())
        })?,
    )?;
    let runtime_clone = context.clone();
    cut_scene.set(
        "intro",
        lua.create_function(move |_, ()| {
            runtime_clone
                .borrow_mut()
                .log_event("cut_scene.intro scheduled");
            Ok(())
        })?,
    )?;
    globals.set("cut_scene", cut_scene)?;

    Ok(())
}

fn install_runtime_tables(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let system = lua.create_table()?;
    system.set("setTable", lua.create_table()?)?;
    system.set("setCount", 0)?;
    globals.set("system", system.clone())?;

    let manny = lua.create_table()?;
    manny.set("name", "Manny")?;
    manny.set("hActor", 1001)?;

    let set_selected_system_key: RegistryKey = lua.create_registry_value(system.clone())?;
    let set_selected_manny_key: RegistryKey = lua.create_registry_value(manny.clone())?;
    let manny_state = context.clone();
    manny.set(
        "set_selected",
        lua.create_function(move |lua_ctx, _args: Variadic<Value>| {
            {
                let mut ctx = manny_state.borrow_mut();
                ctx.select_actor("manny", "Manny");
            }
            let system: Table = lua_ctx.registry_value(&set_selected_system_key)?;
            let manny_table: Table = lua_ctx.registry_value(&set_selected_manny_key)?;
            system.set("currentActor", manny_table.clone())?;
            Ok(())
        })?,
    )?;

    let manny_state = context.clone();
    manny.set(
        "default",
        lua.create_function(move |_, args: Variadic<Value>| {
            if let Some(Value::String(costume)) = args.get(0) {
                manny_state.borrow_mut().set_actor_costume(
                    "manny",
                    "Manny",
                    Some(costume.to_str()?.to_string()),
                );
            }
            Ok(())
        })?,
    )?;

    let manny_state = context.clone();
    let put_in_set_system_key: RegistryKey = lua.create_registry_value(system.clone())?;
    let put_in_set_manny_key: RegistryKey = lua.create_registry_value(manny.clone())?;
    manny.set(
        "put_in_set",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            if let Some(Value::Table(set)) = args.get(0) {
                let set_file: String = set.get("setFile")?;
                manny_state
                    .borrow_mut()
                    .put_actor_in_set("manny", "Manny", &set_file);
            } else if let Some(Value::String(set_file)) = args.get(0) {
                manny_state
                    .borrow_mut()
                    .put_actor_in_set("manny", "Manny", set_file.to_str()?);
            }
            // Keep system.currentActor in sync if absent
            let system: Table = lua_ctx.registry_value(&put_in_set_system_key)?;
            if let Ok(Value::Nil) = system.get::<_, Value>("currentActor") {
                let manny_table: Table = lua_ctx.registry_value(&put_in_set_manny_key)?;
                system.set("currentActor", manny_table)?;
            }
            Ok(())
        })?,
    )?;
    let manny_state = context.clone();
    manny.set(
        "put_at_interest",
        lua.create_function(move |_, _args: Variadic<Value>| {
            manny_state.borrow_mut().actor_at_interest("manny", "Manny");
            Ok(())
        })?,
    )?;
    let manny_state = context.clone();
    manny.set(
        "setpos",
        lua.create_function(move |_, args: Variadic<Value>| {
            if let Some(position) = value_slice_to_vec3(&args) {
                manny_state
                    .borrow_mut()
                    .set_actor_position("manny", "Manny", position);
            }
            Ok(())
        })?,
    )?;
    let manny_state = context.clone();
    manny.set(
        "setrot",
        lua.create_function(move |_, args: Variadic<Value>| {
            if let Some(rotation) = value_slice_to_vec3(&args) {
                manny_state
                    .borrow_mut()
                    .set_actor_rotation("manny", "Manny", rotation);
            }
            Ok(())
        })?,
    )?;
    manny.set(
        "play_chore",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;
    manny.set(
        "pop_costume",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;
    manny.set(
        "head_look_at",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;
    let manny_state = context.clone();
    manny.set(
        "is_speaking",
        lua.create_function(move |_, ()| {
            let actors = &manny_state.borrow().actors;
            let speaking = actors
                .get("manny")
                .map(|actor| actor.is_selected)
                .unwrap_or(false);
            Ok(speaking)
        })?,
    )?;
    let manny_state = context.clone();
    manny.set(
        "getpos",
        lua.create_function(move |lua_ctx, ()| {
            let table = lua_ctx.create_table()?;
            if let Some(actor) = manny_state.borrow().actors.get("manny") {
                if let Some(pos) = actor.position {
                    table.set("x", pos.x)?;
                    table.set("y", pos.y)?;
                    table.set("z", pos.z)?;
                    return Ok(table);
                }
            }
            table.set("x", 0.0)?;
            table.set("y", 0.0)?;
            table.set("z", 0.0)?;
            Ok(table)
        })?,
    )?;
    globals.set("manny", manny.clone())?;

    let mo = lua.create_table()?;
    mo.set("name", "Manny's Office")?;
    let scythe = lua.create_table()?;
    let scythe_state = context.clone();
    scythe.set("name", "mo.scythe")?;
    scythe.set(
        "get",
        lua.create_function(move |_, _args: Variadic<Value>| {
            scythe_state.borrow_mut().add_inventory_item("mo.scythe");
            Ok(())
        })?,
    )?;
    scythe.set("owner", Value::Nil)?;
    mo.set("scythe", scythe)?;
    globals.set("mo", mo)?;

    Ok(())
}

fn value_slice_to_vec3(values: &[Value]) -> Option<Vec3> {
    if values.len() >= 3 {
        let x = value_to_f32(&values[0])?;
        let y = value_to_f32(&values[1])?;
        let z = value_to_f32(&values[2])?;
        return Some(Vec3 { x, y, z });
    }
    if let Some(Value::Table(table)) = values.get(0) {
        let x = table.get::<_, f32>("x").ok()?;
        let y = table.get::<_, f32>("y").ok()?;
        let z = table.get::<_, f32>("z").ok()?;
        return Some(Vec3 { x, y, z });
    }
    None
}

fn value_to_f32(value: &Value) -> Option<f32> {
    match value {
        Value::Integer(i) => Some(*i as f32),
        Value::Number(n) => Some(*n as f32),
        _ => None,
    }
}

fn load_system_script(lua: &Lua, data_root: &Path) -> Result<()> {
    let system_path = data_root.join("_system.decompiled.lua");
    let source = fs::read_to_string(&system_path)
        .with_context(|| format!("reading {}", system_path.display()))?;
    let normalized = normalize_legacy_lua(&source);
    let chunk = lua.load(&normalized).set_name("_system.decompiled.lua");
    chunk.exec().context("executing _system.decompiled.lua")?;
    Ok(())
}

fn override_boot_stubs(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let system: Table = globals.get("system")?;
    let set_table: Table = system.get("setTable")?;

    let mo: Table = globals.get("mo")?;
    mo.set("setFile", "mo.set")?;
    let mo_key = lua.create_registry_value(mo.clone())?;
    let switch_context = context.clone();
    mo.set(
        "switch_to_set",
        lua.create_function(move |lua_ctx, _args: Variadic<Value>| {
            {
                let mut ctx = switch_context.borrow_mut();
                ctx.mark_set_loaded("mo.set");
                ctx.switch_to_set("mo.set");
            }
            let system: Table = lua_ctx.globals().get("system")?;
            let set_ref: Table = lua_ctx.registry_value(&mo_key)?;
            system.set("currentSet", set_ref)?;
            Ok(())
        })?,
    )?;
    set_table.set("mo.set", mo.clone())?;
    context.borrow_mut().mark_set_loaded("mo.set");

    let source_context = context.clone();
    let source_stub = lua.create_function(move |lua_ctx, ()| {
        source_context.borrow_mut().mark_set_loaded("mo.set");
        let system: Table = lua_ctx.globals().get("system")?;
        system.set("setCount", 1)?;
        Ok(())
    })?;
    globals.set("source_all_set_files", source_stub)?;

    globals.set("start_script", create_start_script(lua, context.clone())?)?;
    globals.set(
        "single_start_script",
        create_single_start_script(lua, context.clone())?,
    )?;

    let wait_context = context.clone();
    globals.set(
        "wait_for_script",
        lua.create_function(move |_lua_ctx, args: Variadic<Value>| {
            for value in args.iter() {
                match value {
                    Value::Integer(handle) => {
                        wait_context.borrow_mut().complete_script(*handle as u32);
                    }
                    Value::Number(handle) => {
                        wait_context.borrow_mut().complete_script(*handle as u32);
                    }
                    Value::Function(func) => {
                        func.call::<_, ()>(MultiValue::new())?;
                    }
                    Value::Table(table) => {
                        if let Ok(func) = table.get::<_, Function>("run") {
                            func.call::<_, ()>(())?;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        })?,
    )?;

    let find_context = context.clone();
    globals.set(
        "find_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            if let Some(Value::String(label)) = args.get(0) {
                if let Some(handle) = find_context.borrow().find_script_handle(label.to_str()?) {
                    return Ok(Value::Integer(handle as i64));
                }
            }
            Ok(Value::Nil)
        })?,
    )?;

    // Provide stub for mo.tube referenced during boot
    let mo: Table = globals.get("mo")?;
    let tube = lua.create_table()?;
    tube.set("contains", Value::Nil)?;
    tube.set("is_open", lua.create_function(|_, ()| Ok(false))?)?;
    tube.set(
        "set_object_state",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;
    tube.set("interest_actor", {
        let actor = lua.create_table()?;
        actor.set(
            "complete_chore",
            lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
        )?;
        actor
    })?;
    mo.set("tube", tube)?;
    mo.set(
        "add_object_state",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;
    mo.set("computer", {
        let table = lua.create_table()?;
        table.set(
            "set_object_state",
            lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
        )?;
        table
    })?;
    mo.set("ha_door", {
        let table = lua.create_table()?;
        table.set(
            "set_object_state",
            lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
        )?;
        table
    })?;
    mo.set("cards", {
        let table = lua.create_table()?;
        table.set("owner", "IN_THE_ROOM")?;
        table.set(
            "set_object_state",
            lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
        )?;
        table.set(
            "play_chore",
            lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
        )?;
        let ia = lua.create_table()?;
        ia.set(
            "setrot",
            lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
        )?;
        table.set("interest_actor", ia)?;
        table
    })?;

    Ok(())
}

fn call_boot(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let boot: Function = globals
        .get("BOOT")
        .context("BOOT function missing after loading _system")?;
    boot.call::<_, ()>((false, Value::Nil))
        .context("executing BOOT(false)")?;
    if context.borrow().verbose {
        println!("[lua-runtime] BOOT completed");
    }
    Ok(())
}

fn dump_runtime_summary(state: &EngineContext) {
    println!("Lua runtime summary:");
    match &state.current_set {
        Some(set) => {
            let display = set.display_name.as_deref().unwrap_or(&set.variable_name);
            println!("  Current set: {} ({})", set.set_file, display);
        }
        None => println!("  Current set: <none>"),
    }
    println!(
        "  Selected actor: {}",
        state.selected_actor.as_deref().unwrap_or("<none>")
    );
    if let Some(manny) = state.actors.get("manny") {
        if let Some(set) = &manny.current_set {
            println!("  Manny in set: {set}");
        }
        if let Some(costume) = &manny.costume {
            println!("  Manny costume: {costume}");
        }
        if let Some(pos) = manny.position {
            println!(
                "  Manny position: ({:.3}, {:.3}, {:.3})",
                pos.x, pos.y, pos.z
            );
        }
        if let Some(rot) = manny.rotation {
            println!(
                "  Manny rotation: ({:.3}, {:.3}, {:.3})",
                rot.x, rot.y, rot.z
            );
        }
    }
    if !state.inventory.is_empty() {
        let mut items: Vec<_> = state.inventory.iter().collect();
        items.sort();
        let display = items
            .iter()
            .map(|item| item.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  Inventory: {}", display);
    }
    if !state.scripts.is_empty() {
        println!("  Pending scripts:");
        for (handle, record) in &state.scripts {
            println!("    - {} (#{handle})", record.label);
        }
    }
    if !state.events.is_empty() {
        println!("  Event log:");
        for event in &state.events {
            println!("    - {event}");
        }
    }
}

fn create_start_script(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<Function<'_>> {
    let start_state = context.clone();
    let func = lua.create_function(move |lua_ctx, mut args: Variadic<Value>| {
        if args.is_empty() {
            return Ok(0u32);
        }
        let callable = args.remove(0);
        let label = match &callable {
            Value::Function(_) => "<function>".to_string(),
            Value::String(s) => s.to_str()?.to_string(),
            Value::Table(_) => "<table>".to_string(),
            other => format!("<{other:?}>"),
        };
        let handle = {
            let mut state = start_state.borrow_mut();
            state.start_script(label.clone())
        };
        if let Some(func) = extract_function(lua_ctx, callable)? {
            let params: Vec<Value> = args.into_iter().collect();
            if !params.is_empty() {
                let mv = MultiValue::from_vec(params);
                func.call::<_, ()>(mv)?;
            } else {
                func.call::<_, ()>(MultiValue::new())?;
            }
        }
        start_state.borrow_mut().complete_script(handle);
        Ok(handle)
    })?;
    Ok(func)
}

fn create_single_start_script(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
) -> Result<Function<'_>> {
    let single_state = context.clone();
    let func = lua.create_function(move |lua_ctx, mut args: Variadic<Value>| {
        if args.is_empty() {
            return Ok(0u32);
        }
        let callable = args.remove(0);
        let label = match &callable {
            Value::Function(_) => "<function>".to_string(),
            Value::String(s) => s.to_str()?.to_string(),
            Value::Table(_) => "<table>".to_string(),
            other => format!("<{other:?}>"),
        };
        if single_state.borrow().has_script_with_label(&label) {
            return Ok(0u32);
        }
        let handle = {
            let mut state = single_state.borrow_mut();
            state.start_script(label.clone())
        };
        if let Some(func) = extract_function(lua_ctx, callable)? {
            let params: Vec<Value> = args.into_iter().collect();
            if !params.is_empty() {
                let mv = MultiValue::from_vec(params);
                func.call::<_, ()>(mv)?;
            } else {
                func.call::<_, ()>(MultiValue::new())?;
            }
        }
        single_state.borrow_mut().complete_script(handle);
        Ok(handle)
    })?;
    Ok(func)
}

fn extract_function<'lua>(lua: &'lua Lua, value: Value<'lua>) -> LuaResult<Option<Function<'lua>>> {
    match value {
        Value::Function(f) => Ok(Some(f)),
        Value::String(name) => {
            let globals = lua.globals();
            let func: Function = globals.get(name.to_str()?)?;
            Ok(Some(func))
        }
        Value::Table(table) => {
            if let Ok(func) = table.get::<_, Function>("run") {
                Ok(Some(func))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::{candidate_paths, value_slice_to_vec3};
    use mlua::Value;
    use std::path::PathBuf;

    #[test]
    fn candidate_paths_cover_decompiled_variants() {
        let mut paths = candidate_paths("setfallback.lua");
        paths.sort();
        assert!(paths.contains(&PathBuf::from("setfallback.lua")));
        assert!(paths.contains(&PathBuf::from("setfallback.decompiled.lua")));
        assert!(paths.contains(&PathBuf::from("Scripts/setfallback.lua")));
    }

    #[test]
    fn value_slice_to_vec3_reads_numeric_values() {
        let values = vec![Value::Number(1.0), Value::Integer(2), Value::Number(3.5)];
        let vec = value_slice_to_vec3(&values).expect("vector parsed");
        assert!((vec.x - 1.0).abs() < f32::EPSILON);
        assert!((vec.y - 2.0).abs() < f32::EPSILON);
        assert!((vec.z - 3.5).abs() < f32::EPSILON);
    }
}
