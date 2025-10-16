#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHookName {
    pub trimmed: String,
    pub normalized: String,
    pub simplified: String,
}

pub fn normalize_hook_name(input: &str) -> Option<NormalizedHookName> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.to_ascii_lowercase();
    let simplified: String = normalized.chars().filter(|c| *c != '_').collect();
    Some(NormalizedHookName {
        trimmed: trimmed.to_string(),
        normalized,
        simplified,
    })
}

#[cfg(test)]
mod tests {
    use super::normalize_hook_name;

    #[test]
    fn normalize_basic_values() {
        let normalized = normalize_hook_name(" Set_Up_Meche ").expect("normalized");
        assert_eq!(normalized.trimmed, "Set_Up_Meche");
        assert_eq!(normalized.normalized, "set_up_meche");
        assert_eq!(normalized.simplified, "setupmeche");
    }

    #[test]
    fn normalize_rejects_blank_input() {
        assert!(normalize_hook_name("   ").is_none());
    }
}
