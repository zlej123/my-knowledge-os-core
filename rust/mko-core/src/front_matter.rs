use std::collections::HashSet;

use serde::{Serialize, de::DeserializeOwned};
use unicode_normalization::UnicodeNormalization;

use crate::{
    error::MkoError,
    safe_yaml::{MAX_YAML_BYTES, validate_yaml_input},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedMarkdown<T> {
    pub metadata: T,
    pub body: String,
}

pub fn parse_markdown<T>(input: &str) -> Result<ParsedMarkdown<T>, MkoError>
where
    T: DeserializeOwned,
{
    let raw_yaml = raw_front_matter(input)?;
    if raw_yaml.len() > MAX_YAML_BYTES {
        return Err(MkoError::new("unsafe_yaml", "front matter exceeds 256 KiB"));
    }

    let normalized = normalize_lf(input);
    let Some(remaining) = normalized.strip_prefix("---\n") else {
        return Err(MkoError::new(
            "front_matter_invalid",
            "front matter must begin with ---",
        ));
    };
    let Some(closing_offset) = delimiter_offset(remaining) else {
        return Err(MkoError::new(
            "front_matter_invalid",
            "front matter must end with ---",
        ));
    };
    let yaml = &remaining[..closing_offset];
    let body_start = closing_offset + delimiter_len(&remaining[closing_offset..]);
    let body = &remaining[body_start..];
    if body.starts_with("---\n") || body == "---" {
        return Err(MkoError::new(
            "yaml_invalid",
            "multiple YAML documents are forbidden",
        ));
    }

    validate_yaml_input(yaml)?;
    reject_duplicate_top_level_keys(yaml)?;
    let metadata = serde_saphyr::from_str(yaml).map_err(|error| {
        let category = error.to_string();
        if category.contains("duplicate field") || category.contains("duplicate mapping key") {
            MkoError::new("yaml_invalid", "front matter contains a duplicate YAML key")
        } else {
            MkoError::new(
                "schema_invalid",
                "front matter does not match the required schema",
            )
        }
    })?;

    Ok(ParsedMarkdown {
        metadata,
        body: body.nfc().collect(),
    })
}

fn raw_front_matter(input: &str) -> Result<&str, MkoError> {
    let remaining = if let Some(remaining) = input.strip_prefix("---\r\n") {
        remaining
    } else if let Some(remaining) = input.strip_prefix("---\n") {
        remaining
    } else {
        return Err(MkoError::new(
            "front_matter_invalid",
            "front matter must begin with ---",
        ));
    };
    let Some(closing_offset) = raw_delimiter_offset(remaining) else {
        return Err(MkoError::new(
            "front_matter_invalid",
            "front matter must end with ---",
        ));
    };
    Ok(&remaining[..closing_offset])
}

pub fn render_markdown<T>(metadata: &T, body: &str) -> Result<String, MkoError>
where
    T: Serialize,
{
    let yaml = serde_saphyr::to_string(metadata)
        .map_err(|error| MkoError::new("yaml_invalid", error.to_string()))?;
    let yaml = normalize_lf(&yaml);
    let yaml = yaml.trim_end_matches('\n');
    let body = normalize_lf(body);

    Ok(format!(
        "---\n{}\n---\n{}",
        yaml.nfc().collect::<String>(),
        body.nfc()
    ))
}

fn delimiter_offset(input: &str) -> Option<usize> {
    let mut offset = 0;
    for line in input.split_inclusive('\n') {
        if line.trim_end_matches('\n') == "---" {
            return Some(offset);
        }
        offset += line.len();
    }
    if input == "---" { Some(0) } else { None }
}

fn delimiter_len(input: &str) -> usize {
    input.find('\n').map_or(3, |index| index + 1)
}

fn raw_delimiter_offset(input: &str) -> Option<usize> {
    let mut offset = 0;
    for line in input.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some(offset);
        }
        offset += line.len();
    }
    if input == "---" { Some(0) } else { None }
}

fn reject_duplicate_top_level_keys(input: &str) -> Result<(), MkoError> {
    let mut keys = HashSet::new();
    for line in input.lines() {
        if line.is_empty()
            || line.starts_with(char::is_whitespace)
            || line.trim_start().starts_with('#')
        {
            continue;
        }
        let Some((key, _)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.starts_with('-') || key.starts_with('?') {
            continue;
        }
        if !keys.insert(key.to_owned()) {
            return Err(MkoError::new(
                "yaml_invalid",
                "duplicate top-level YAML key",
            ));
        }
    }
    Ok(())
}

fn normalize_lf(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}
