use grim_telemetry_common::OriginFields;
use mlua::{FromLua, Lua, RegistryKey, Result as LuaResult, Value};
use std::collections::HashMap;

use crate::lua_host::telemetry::{
    log_load_ref, log_store_ref, normalize_handle, register_table_label,
};

use super::util::{handle_from_value, value_fields_from_lua};

#[derive(Debug)]
pub(crate) struct RegistryRef {
    pub reference: i32,
    pub key: RegistryKey,
    pub handle: Option<String>,
    pub label: Option<String>,
}

impl RegistryRef {
    pub(crate) fn log_fetch(&self, origin: OriginFields, note: Option<String>) {
        log_load_ref(
            self.reference,
            self.handle.clone(),
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
    if let Value::Table(table) = &value {
        if let Some(label) = handle_label.clone().or(label.clone()) {
            register_table_label(table.to_pointer(), label);
        }
    }
    let value_fields = value_fields_from_lua(&value);
    let key = lua.create_registry_value(value)?;
    let reference = reference.unwrap_or_else(|| key.id());
    log_store_ref(
        lock,
        reference,
        Some(handle.clone()),
        label.clone(),
        Some(value_fields),
    );
    Ok(RegistryRef {
        reference,
        key,
        handle: Some(handle),
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

pub(crate) struct RefRegistry {
    entries: HashMap<i32, RegistryRef>,
    next: i32,
}

impl RefRegistry {
    pub(crate) fn new() -> Self {
        Self {
            entries: HashMap::new(),
            next: 1,
        }
    }

    fn next_handle(&mut self) -> i32 {
        let handle = self.next.max(1);
        self.next = handle.saturating_add(1);
        handle
    }

    fn reserve_reference(&mut self, preferred: Option<i32>) -> i32 {
        match preferred {
            Some(ref_id) if ref_id > 0 => {
                self.next = self.next.max(ref_id.saturating_add(1));
                ref_id
            }
            _ => self.next_handle(),
        }
    }

    pub(crate) fn alloc_ref<'lua>(
        &mut self,
        lua: &'lua Lua,
        value: Value<'lua>,
        preferred_ref: Option<i32>,
        label: Option<String>,
        preferred_handle: Option<String>,
        handle_label: Option<String>,
    ) -> LuaResult<i32> {
        let reference = self.reserve_reference(preferred_ref);
        let entry = store_registry_value(
            lua,
            value,
            1,
            Some(reference),
            label,
            preferred_handle,
            handle_label,
        )?;
        self.entries.insert(reference, entry);
        Ok(reference)
    }

    pub(crate) fn fetch_ref<'lua, T: FromLua<'lua>>(
        &self,
        lua: &'lua Lua,
        reference: i32,
        origin: OriginFields,
        note_on_missing: Option<String>,
    ) -> LuaResult<Option<T>> {
        if let Some(entry) = self.entries.get(&reference) {
            entry.log_fetch(origin.clone(), None);
            let value: Value = lua.registry_value(&entry.key)?;
            return T::from_lua(value, lua).map(Some);
        }
        log_load_ref(reference, None, None, note_on_missing, origin);
        Ok(None)
    }

    pub(crate) fn remove(&mut self, reference: i32) -> bool {
        self.entries.remove(&reference).is_some()
    }
}
