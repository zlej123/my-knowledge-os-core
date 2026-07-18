use granit_parser::{Event, Parser};

use crate::error::MkoError;

pub const MAX_YAML_BYTES: usize = 256 * 1024;
pub const MAX_YAML_DEPTH: usize = 32;

pub fn validate_yaml_input(input: &str) -> Result<(), MkoError> {
    if input.len() > MAX_YAML_BYTES {
        return Err(MkoError::new("unsafe_yaml", "front matter exceeds 256 KiB"));
    }

    let mut depth = 0usize;
    for event in Parser::new_from_str(input) {
        let (event, _) = event.map_err(|_| {
            MkoError::new("yaml_invalid", "front matter contains invalid YAML syntax")
        })?;
        match event {
            Event::Alias(_) => {
                return Err(MkoError::new(
                    "unsafe_yaml",
                    "YAML tags, anchors, and aliases are forbidden",
                ));
            }
            Event::Scalar(_, _, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    return Err(MkoError::new(
                        "unsafe_yaml",
                        "YAML tags, anchors, and aliases are forbidden",
                    ));
                }
            }
            Event::SequenceStart(_, anchor, tag) | Event::MappingStart(_, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    return Err(MkoError::new(
                        "unsafe_yaml",
                        "YAML tags, anchors, and aliases are forbidden",
                    ));
                }
                depth += 1;
                if depth > MAX_YAML_DEPTH {
                    return Err(MkoError::new(
                        "unsafe_yaml",
                        "front matter exceeds maximum YAML nesting depth",
                    ));
                }
            }
            Event::SequenceEnd | Event::MappingEnd => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }

    Ok(())
}
