use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::components::Paths;
use crate::lifecycle::build_ld_library_path;
use crate::retail::RetailLayout;

pub(crate) const LUA_NEWSTATE_OFF: u32 = 0x125f0;
pub(crate) const LUA_OPEN_OFF: u32 = 0x128a0;
pub(crate) const LUA_TYPE_NAMES: &[(i32, &str)] = &[
    (0, "userdata"),
    (-1, "number"),
    (-2, "string"),
    (-3, "array"),
    (-4, "proto"),
    (-5, "cproto"),
    (-6, "nil"),
    (-7, "closure"),
    (-8, "clmark"),
    (-9, "pmark"),
    (-10, "cmark"),
    (-11, "line"),
];
pub(crate) const LUA_POINTER_HINTS: &[(i32, &str)] = &[
    (0, "userdata"),
    (-2, "string"),
    (-3, "table/array"),
    (-4, "proto"),
    (-5, "cproto"),
    (-7, "closure"),
    (-8, "clmark"),
    (-9, "pmark"),
    (-10, "cmark"),
];

const GDB_PY_TEMPLATE: &str = include_str!("gdb_template.py");

fn format_python_env_map(pairs: &[(String, String)]) -> String {
    let mut env = String::from("{\n");
    for (key, value) in pairs {
        env.push_str("    ");
        env.push_str(&format!("{:?}: {:?},\n", key, value));
    }
    env.push('}');
    env
}

fn format_lua_type_names() -> String {
    let mut names = String::from("{\n");
    for (ttype, name) in LUA_TYPE_NAMES {
        names.push_str(&format!("    {ttype}: '{name}',\n"));
    }
    names.push('}');
    names
}

fn format_pointer_hints() -> String {
    let mut names = String::from("{\n");
    for (ttype, name) in LUA_POINTER_HINTS {
        names.push_str(&format!("    {ttype}: '{name}',\n"));
    }
    names.push('}');
    names
}

fn render_gdb_python(
    liblua_path: &str,
    shim_path: Option<&str>,
    env_pairs: &[(String, String)],
    env_preload: Option<&str>,
) -> String {
    let shim_value = shim_path
        .map(|path| format!("{:?}", path))
        .unwrap_or_else(|| "None".to_string());
    let env_preload_value = env_preload
        .map(|path| format!("{:?}", path))
        .unwrap_or_else(|| "None".to_string());

    let mut script = GDB_PY_TEMPLATE.replace("__LIBLUA_PATH__", &format!("{:?}", liblua_path));
    script = script.replace("__LUA_NEWSTATE_OFF__", &format!("{:#x}", LUA_NEWSTATE_OFF));
    script = script.replace("__LUA_OPEN_OFF__", &format!("{:#x}", LUA_OPEN_OFF));
    script = script.replace("__LUA_TYPE_NAMES__", &format_lua_type_names());
    script = script.replace("__POINTER_HINTS__", &format_pointer_hints());
    script = script.replace("__SHIM_PATH__", &shim_value);
    script = script.replace("__ENV_VARS__", &format_python_env_map(env_pairs));
    script = script.replace("__ENV_PRELOAD__", &env_preload_value);
    script
}

fn gdb_solib_search_path(layout: &RetailLayout) -> Option<String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    paths.push(layout.dev_install().to_path_buf());
    let dev_install_x86 = layout.dev_install().join("x86");
    if dev_install_x86.exists() {
        paths.push(dev_install_x86);
    }
    if let Some(ld_path) = build_ld_library_path(layout) {
        for entry in ld_path.split(':') {
            if !entry.is_empty() {
                paths.push(PathBuf::from(entry));
            }
        }
    }
    if let Some(shim) = layout.resolved_shim_path() {
        if let Some(parent) = shim.parent() {
            paths.push(parent.to_path_buf());
        }
    }
    paths.retain(|p| p.exists());
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        None
    } else {
        let value = paths
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":");
        Some(value)
    }
}

pub(crate) fn write_retail_gdb_script(
    paths: &Paths,
    layout: &RetailLayout,
    session_id: &str,
    env_pairs: &[(String, String)],
    ld_preload: Option<&str>,
) -> Result<PathBuf> {
    let cmd_path = paths.gdb_commands_path(session_id);
    let mut file = File::create(&cmd_path)
        .with_context(|| format!("creating gdb command file {}", cmd_path.display()))?;
    let liblua_path = layout.liblua_bin().to_string_lossy().into_owned();
    let shim_path = layout
        .resolved_shim_path()
        .map(|path| path.to_string_lossy().into_owned());
    let filtered_env_pairs: Vec<(String, String)> = env_pairs
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "LD_PRELOAD" | "LD_PRELOAD_32"))
        .cloned()
        .collect();
    let python = render_gdb_python(
        &liblua_path,
        shim_path.as_deref(),
        &filtered_env_pairs,
        ld_preload,
    );

    writeln!(file, "set pagination off")?;
    writeln!(file, "set confirm off")?;
    writeln!(file, "set breakpoint pending on")?;
    writeln!(file, "set architecture i386")?;
    writeln!(file, "set disable-randomization on")?;
    writeln!(file, "set sysroot /")?;
    if let Some(solib) = gdb_solib_search_path(layout) {
        writeln!(file, "set solib-search-path {}", solib)?;
    }
    writeln!(file, "file {}", layout.retail_bin().to_string_lossy())?;
    writeln!(file, "cd {}", layout.dev_install().to_string_lossy())?;

    writeln!(file, "python")?;
    file.write_all(python.as_bytes())?;
    if !python.ends_with('\n') {
        writeln!(file)?;
    }
    writeln!(file, "end")?;
    writeln!(file, "python apply_env()")?;
    writeln!(file, "start")?;
    writeln!(file, "python load_symbols()")?;
    writeln!(file, "python set_lua_alloc_breaks()")?;
    writeln!(
        file,
        "echo [grctl] gdb ready (session {}) — stopped at entry; Lua breakpoints set; 'continue' to run.\\n",
        session_id
    )?;
    Ok(cmd_path)
}
