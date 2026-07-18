use crate::error::MkoError;

pub const MAX_YAML_BYTES: usize = 256 * 1024;
pub const MAX_YAML_DEPTH: usize = 32;

pub fn validate_yaml_input(input: &str) -> Result<(), MkoError> {
    if input.len() > MAX_YAML_BYTES {
        return Err(MkoError::new("unsafe_yaml", "front matter exceeds 256 KiB"));
    }
    for line in input.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('!')
            || trimmed.contains(" &")
            || trimmed.contains(" *")
            || contains_yaml_tag(trimmed)
        {
            return Err(MkoError::new(
                "unsafe_yaml",
                "YAML tags, anchors, and aliases are forbidden",
            ));
        }
    }

    validate_depth(input)
}

fn validate_depth(input: &str) -> Result<(), MkoError> {
    let mut indentation_stack = Vec::new();
    let mut flow_depth = 0usize;

    for line in input.lines() {
        let content = line.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }

        let indentation = line.len() - content.len();
        while indentation_stack
            .last()
            .is_some_and(|previous| *previous > indentation)
        {
            indentation_stack.pop();
        }
        if indentation_stack
            .last()
            .is_none_or(|previous| *previous < indentation)
        {
            indentation_stack.push(indentation);
        }

        let compact_sequence_depth = content
            .split_whitespace()
            .filter(|token| *token == "-")
            .count();
        if indentation_stack
            .len()
            .saturating_sub(1)
            .saturating_add(compact_sequence_depth)
            > MAX_YAML_DEPTH
        {
            return Err(MkoError::new(
                "unsafe_yaml",
                "front matter exceeds maximum YAML nesting depth",
            ));
        }

        for character in content.chars() {
            match character {
                '[' | '{' => flow_depth += 1,
                ']' | '}' => flow_depth = flow_depth.saturating_sub(1),
                _ => {}
            }
            if flow_depth > MAX_YAML_DEPTH {
                return Err(MkoError::new(
                    "unsafe_yaml",
                    "front matter exceeds maximum YAML nesting depth",
                ));
            }
        }
    }

    Ok(())
}

fn contains_yaml_tag(input: &str) -> bool {
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut escaped = false;
    let mut previous = None;

    for character in input.chars() {
        if double_quoted && character == '\\' && !escaped {
            escaped = true;
            previous = Some(character);
            continue;
        }
        if character == '\'' && !double_quoted {
            single_quoted = !single_quoted;
        } else if character == '"' && !single_quoted && !escaped {
            double_quoted = !double_quoted;
        } else if character == '!'
            && !single_quoted
            && !double_quoted
            && previous.is_none_or(|previous: char| {
                previous.is_whitespace() || matches!(previous, ':' | '[' | '{' | ',' | '-')
            })
        {
            return true;
        }
        escaped = false;
        previous = Some(character);
    }

    false
}
