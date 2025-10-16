use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::state_catalog::CatalogCoverage;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CoverageFile {
    Map(HashMap<String, u64>),
    Wrapper { counts: HashMap<String, u64> },
}

impl CoverageFile {
    fn into_counts(self) -> HashMap<String, u64> {
        match self {
            CoverageFile::Map(map) => map,
            CoverageFile::Wrapper { counts } => counts,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CoverageComparison {
    pub missing: Vec<String>,
    pub unexpected: Vec<String>,
    pub covered: Vec<String>,
}

pub fn load_coverage_counts(path: &Path) -> Result<HashMap<String, u64>> {
    let file =
        File::open(path).with_context(|| format!("opening coverage file {}", path.display()))?;
    let coverage: CoverageFile = serde_json::from_reader(file)
        .with_context(|| format!("parsing coverage file {}", path.display()))?;
    Ok(coverage.into_counts())
}

pub fn compare_catalog_with_coverage(
    catalog: &CatalogCoverage,
    counts: &HashMap<String, u64>,
) -> CoverageComparison {
    let catalog_keys: BTreeSet<&String> = catalog.keys.iter().collect();
    let coverage_keys: BTreeSet<&String> = counts.keys().collect();

    let missing = catalog_keys
        .difference(&coverage_keys)
        .map(|key| (*key).clone())
        .collect();
    let unexpected = coverage_keys
        .difference(&catalog_keys)
        .map(|key| (*key).clone())
        .collect();
    let covered = catalog_keys
        .intersection(&coverage_keys)
        .filter(|key| counts.get(key.as_str()).copied().unwrap_or_default() > 0)
        .map(|key| (*key).clone())
        .collect();

    CoverageComparison {
        missing,
        unexpected,
        covered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_wrapper_parses() {
        let json = r#"{ "counts": { "actor:manny": 1 } }"#;
        let coverage: CoverageFile = serde_json::from_str(json).unwrap();
        let counts = coverage.into_counts();
        assert_eq!(counts.get("actor:manny"), Some(&1));
    }

    #[test]
    fn coverage_map_parses() {
        let json = r#"{ "actor:manny": 3, "set:mo": 0 }"#;
        let coverage: CoverageFile = serde_json::from_str(json).unwrap();
        let counts = coverage.into_counts();
        assert_eq!(counts.get("actor:manny"), Some(&3));
        assert_eq!(counts.get("set:mo"), Some(&0));
    }

    #[test]
    fn comparison_reports_missing_and_unexpected() {
        let catalog = CatalogCoverage {
            keys: vec!["actor:manny".into(), "set:mo".into()],
        };
        let mut counts = HashMap::new();
        counts.insert("actor:manny".into(), 1);
        counts.insert("script:year:year1.lua".into(), 1);

        let comparison = compare_catalog_with_coverage(&catalog, &counts);
        assert_eq!(comparison.missing, vec!["set:mo".to_string()]);
        assert_eq!(
            comparison.unexpected,
            vec!["script:year:year1.lua".to_string()]
        );
        assert_eq!(comparison.covered, vec!["actor:manny".to_string()]);
    }
}
