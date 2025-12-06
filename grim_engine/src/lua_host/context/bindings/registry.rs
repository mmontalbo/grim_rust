use grim_telemetry_common::OriginFields;
use mlua::{FromLua, Lua, RegistryKey, Result as LuaResult, Value};

use crate::lua_host::telemetry::{log_fetch_ref, log_store_ref, normalize_handle};

use super::util::handle_from_value;

#[derive(Debug)]
pub(crate) struct RegistryRef {
    pub reference: i32,
    pub key: RegistryKey,
    pub handle: Option<String>,
    pub handle_label: Option<String>,
    pub label: Option<String>,
}

impl RegistryRef {
    pub(crate) fn log_fetch(&self, origin: OriginFields, note: Option<String>) {
        log_fetch_ref(
            self.reference,
            self.handle.clone(),
            self.handle_label.clone(),
            self.label.clone(),
            note,
            origin,
        );
    }

    pub(crate) fn fetch<'lua, T: FromLua<'lua>>(
        &self,
        lua: &'lua Lua,
        origin: OriginFields,
        note: Option<String>,
    ) -> LuaResult<T> {
        self.log_fetch(origin, note.clone());
        lua.registry_value(&self.key)
    }
}

pub(crate) fn store_registry_value<'lua>(
    lua: &'lua Lua,
    value: Value<'lua>,
    lock: i32,
    reference: Option<i32>,
    label: Option<String>,
    preferred_handle: Option<String>,
    handle_label: Option<String>,
) -> LuaResult<RegistryRef> {
    let handle_label = handle_label.or_else(|| label.clone());
    let label_for_handle = handle_label
        .as_deref()
        .or_else(|| label.as_deref())
        .unwrap_or("ref");
    let handle = normalize_handle(
        label_for_handle,
        preferred_handle.or_else(|| handle_from_value(&value)),
    );
    let key = lua.create_registry_value(value)?;
    let reference = reference.unwrap_or_else(|| key.id());
    log_store_ref(
        lock,
        reference,
        Some(handle.clone()),
        handle_label.clone(),
        label.clone(),
    );
    Ok(RegistryRef {
        reference,
        key,
        handle: Some(handle),
        handle_label,
        label,
    })
}

#[derive(Default)]
pub(crate) struct PinnedRegistryKeys {
    keys: Vec<RegistryKey>,
}

impl PinnedRegistryKeys {
    pub(crate) fn push(&mut self, key: RegistryKey) {
        self.keys.push(key);
    }
}

impl Drop for PinnedRegistryKeys {
    fn drop(&mut self) {
        for key in self.keys.drain(..) {
            std::mem::forget(key);
        }
    }
}
