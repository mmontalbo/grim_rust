use serde::{Deserialize, Serialize};

pub(crate) const STACK_DUMP_SCHEMA_VERSION: &str = "v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StackDumpRecord {
    pub seq: u64,
    pub idx: u32,
    #[serde(rename = "type")]
    pub type_name: String,
    pub ttype_hex: String,
    pub v0: u32,
    pub v1: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<StackSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StackSection {
    Head,
    Tail,
}

impl StackDumpRecord {
    pub fn schema_version() -> &'static str {
        STACK_DUMP_SCHEMA_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_sample() {
        let record = StackDumpRecord {
            seq: 42,
            idx: 1,
            type_name: "string".to_string(),
            ttype_hex: "0xdeadbeef".to_string(),
            v0: 0x1,
            v1: 0x2,
            preview: Some("hello".to_string()),
            section: Some(StackSection::Head),
        };
        let json = serde_json::to_string(&record).unwrap();
        let decoded: StackDumpRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, record);
    }
}
