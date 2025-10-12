use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CosFile {
    pub version: String,
    pub tags: Vec<CosTag>,
    pub components: Vec<CosComponent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CosTag {
    pub id: i32,
    pub tag: String,
    pub class: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CosComponent {
    pub id: i32,
    pub tag_id: i32,
    pub hash: i32,
    pub parent_id: i32,
    pub name: String,
}

enum Section {
    None,
    Tags,
    Components,
    Other,
}

impl CosFile {
    pub fn parse_bytes(input: &[u8]) -> Result<Self> {
        let text = String::from_utf8(input.to_vec()).context("costume payload is not UTF-8")?;
        Self::parse_str(&text)
    }

    pub fn parse_str(text: &str) -> Result<Self> {
        let normalized = text.replace("\r\n", "\n");
        let mut version: Option<String> = None;
        let mut tags: Vec<CosTag> = Vec::new();
        let mut components: Vec<CosComponent> = Vec::new();
        let mut section = Section::None;

        for raw_line in normalized.lines() {
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('#') {
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("costume") {
                version = Some(rest.trim().to_string());
                continue;
            }

            if let Some(name) = trimmed.strip_prefix("section") {
                section = match name.trim().to_ascii_lowercase().as_str() {
                    "tags" => Section::Tags,
                    "components" => Section::Components,
                    _ => Section::Other,
                };
                continue;
            }

            let without_comment = strip_inline_comment(trimmed);
            if without_comment.is_empty() {
                continue;
            }

            match section {
                Section::Tags => {
                    if without_comment.starts_with("numtags") {
                        continue;
                    }
                    if let Some(tag) = parse_tag_line(without_comment)? {
                        tags.push(tag);
                    }
                }
                Section::Components => {
                    if without_comment.starts_with("numcomponents") {
                        continue;
                    }
                    if let Some(component) = parse_component_line(without_comment)? {
                        components.push(component);
                    }
                }
                Section::Other | Section::None => {}
            }
        }

        let version = version.ok_or_else(|| anyhow!("missing costume version header"))?;

        Ok(CosFile {
            version,
            tags,
            components,
        })
    }
}

fn parse_tag_line(line: &str) -> Result<Option<CosTag>> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(None);
    }
    if parts.len() < 4 {
        bail!("malformed tag entry: {line}");
    }

    let id = parse_i32(parts[0]).context("parsing tag id")?;
    let tag = parts[1].trim_matches('\'').to_string();
    let class = parts[2].to_string();
    let name = parts[3..].join(" ");

    Ok(Some(CosTag {
        id,
        tag,
        class,
        name,
    }))
}

fn parse_component_line(line: &str) -> Result<Option<CosComponent>> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(None);
    }
    if parts.len() < 5 {
        bail!("malformed component entry: {line}");
    }

    let id = parse_i32(parts[0]).context("parsing component id")?;
    let tag_id = parse_i32(parts[1]).context("parsing component tag id")?;
    let hash = parse_i32(parts[2]).context("parsing component hash")?;
    let parent_id = parse_i32(parts[3]).context("parsing component parent id")?;
    let name = parts[4..].join(" ");

    Ok(Some(CosComponent {
        id,
        tag_id,
        hash,
        parent_id,
        name,
    }))
}

fn parse_i32(value: &str) -> Result<i32> {
    value
        .parse::<i32>()
        .with_context(|| format!("parsing integer '{value}'"))
}

fn strip_inline_comment(line: &str) -> &str {
    match line.find('#') {
        Some(idx) => line[..idx].trim_end(),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_costume() {
        let source = r#"
        # Comment line
        costume v0.1

        section tags
            numtags 2

        #   ID  Tag     Class   Name
            0   'MMDL'  fs      Manny Suit (.3DO)
            1   'KEYF'  fs      Manny Skeleton Keyframe

        section components
            numcomponents 3

        #   ID  TagID   Hash    ParentID    Name
            0   0       0       -1          mannysuit.3do
            1   0       0       0           object_null.3do
            2   1       0       0           ma_idle.key
        "#;

        let parsed = CosFile::parse_str(source).expect("parsed costume");
        assert_eq!(parsed.version, "v0.1");
        assert_eq!(parsed.tags.len(), 2);
        assert_eq!(parsed.components.len(), 3);

        let main_mesh = parsed.components.iter().find(|c| c.id == 0).unwrap();
        assert_eq!(main_mesh.name, "mannysuit.3do");
    }
}
