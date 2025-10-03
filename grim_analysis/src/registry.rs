use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{Map, Number, Value};

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum JsonPrimitive {
    String(String),
    Int(i64),
    Bool(bool),
    Float(f64),
    Null,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RegistryValue {
    String(String),
    Int(i64),
    Bool(bool),
    Float(f64),
    Null,
}

impl From<JsonPrimitive> for RegistryValue {
    fn from(value: JsonPrimitive) -> Self {
        match value {
            JsonPrimitive::String(s) => RegistryValue::String(s),
            JsonPrimitive::Int(i) => RegistryValue::Int(i),
            JsonPrimitive::Bool(b) => RegistryValue::Bool(b),
            JsonPrimitive::Float(f) => RegistryValue::Float(f),
            JsonPrimitive::Null => RegistryValue::Null,
        }
    }
}

/// Simplified stand-in for Grim's registry system used by the boot scripts.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    values: HashMap<String, RegistryValue>,
    dirty: bool,
    backing_path: Option<PathBuf>,
}

impl Registry {
    pub fn from_json_file(path: Option<&Path>) -> Result<Self> {
        let mut registry = Registry {
            values: HashMap::new(),
            dirty: false,
            backing_path: path.map(|p| p.to_path_buf()),
        };
        if let Some(p) = path {
            if p.exists() {
                let raw = fs::read_to_string(p)
                    .with_context(|| format!("failed to read registry file: {}", p.display()))?;
                let map: HashMap<String, JsonPrimitive> = serde_json::from_str(&raw)
                    .with_context(|| format!("failed to parse registry json: {}", p.display()))?;
                registry
                    .values
                    .extend(map.into_iter().map(|(k, v)| (k, RegistryValue::from(v))));
            }
        }
        Ok(registry)
    }

    pub fn read_string(&self, key: &str) -> Option<&str> {
        match self.values.get(key) {
            Some(RegistryValue::String(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn read_int(&self, key: &str) -> Option<i64> {
        match self.values.get(key) {
            Some(RegistryValue::Int(i)) => Some(*i),
            Some(RegistryValue::Float(f)) => Some(*f as i64),
            _ => None,
        }
    }

    pub fn read_bool(&self, key: &str) -> Option<bool> {
        match self.values.get(key) {
            Some(RegistryValue::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn read_float(&self, key: &str) -> Option<f64> {
        match self.values.get(key) {
            Some(RegistryValue::Float(f)) => Some(*f),
            Some(RegistryValue::Int(i)) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn write_string(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.write_value(key.into(), RegistryValue::String(value.into()));
    }

    pub fn write_int(&mut self, key: impl Into<String>, value: i64) {
        self.write_value(key.into(), RegistryValue::Int(value));
    }

    pub fn write_bool(&mut self, key: impl Into<String>, value: bool) {
        self.write_value(key.into(), RegistryValue::Bool(value));
    }

    pub fn write_float(&mut self, key: impl Into<String>, value: f64) {
        self.write_value(key.into(), RegistryValue::Float(value));
    }

    pub fn write_null(&mut self, key: impl Into<String>) {
        self.write_value(key.into(), RegistryValue::Null);
    }

    pub fn remove(&mut self, key: &str) {
        if self.values.remove(key).is_some() {
            self.dirty = true;
        }
    }

    pub fn set_backing_path(&mut self, path: PathBuf) {
        self.backing_path = Some(path);
    }

    pub fn save(&mut self) -> Result<()> {
        let Some(path) = self.backing_path.as_ref() else {
            // No configured backing file; treat as successful no-op.
            self.dirty = false;
            return Ok(());
        };

        if !self.dirty {
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create registry directory: {}", parent.display())
                })?;
            }
        }

        let json_value = Value::Object(self.to_json_map()?);
        let serialized = serde_json::to_string_pretty(&json_value)
            .with_context(|| format!("failed to serialize registry to JSON: {}", path.display()))?;
        fs::write(path, serialized)
            .with_context(|| format!("failed to write registry file: {}", path.display()))?;
        self.dirty = false;
        Ok(())
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        let mut snapshot = self.clone();
        snapshot.set_backing_path(path.to_path_buf());
        snapshot.save()
    }

    fn write_value(&mut self, key: String, value: RegistryValue) {
        let needs_write = match self.values.get(&key) {
            Some(existing) => existing != &value,
            None => true,
        };
        if needs_write {
            self.values.insert(key, value);
            self.dirty = true;
        }
    }

    fn to_json_map(&self) -> Result<Map<String, Value>> {
        let mut map = Map::new();
        for (key, value) in &self.values {
            map.insert(key.clone(), Self::value_to_json(value)?);
        }
        Ok(map)
    }

    fn value_to_json(value: &RegistryValue) -> Result<Value> {
        match value {
            RegistryValue::String(s) => Ok(Value::String(s.clone())),
            RegistryValue::Int(i) => Ok(Value::Number((*i).into())),
            RegistryValue::Bool(b) => Ok(Value::Bool(*b)),
            RegistryValue::Float(f) => Number::from_f64(*f)
                .map(Value::Number)
                .ok_or_else(|| anyhow!("unable to serialize NaN/inf float to JSON")),
            RegistryValue::Null => Ok(Value::Null),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn registry_roundtrip_preserves_values() -> Result<()> {
        let mut registry = Registry::default();
        registry.write_string("hero", "manny");
        registry.write_int("LastSavedGame", 3);
        registry.write_bool("DirectorsCommentary", true);
        registry.write_float("ratio", 1.5);
        registry.write_null("obsolete_key");
        registry.remove("ghost_key");

        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let temp_dir = std::env::temp_dir();
        let alt_path = temp_dir.join(format!("grim_registry_test_{unique_suffix}_alt.json"));
        registry.save_to_path(&alt_path)?;
        let _ = std::fs::remove_file(&alt_path);

        let path = temp_dir.join(format!("grim_registry_test_{unique_suffix}.json"));

        registry.set_backing_path(path.clone());
        registry.save()?;

        let reloaded = Registry::from_json_file(Some(&path))?;
        assert_eq!(reloaded.read_string("hero"), Some("manny"));
        assert_eq!(reloaded.read_int("LastSavedGame"), Some(3));
        assert_eq!(reloaded.read_bool("DirectorsCommentary"), Some(true));
        assert_eq!(reloaded.read_float("ratio"), Some(1.5));
        assert!(matches!(
            reloaded.values.get("obsolete_key"),
            Some(RegistryValue::Null)
        ));

        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
