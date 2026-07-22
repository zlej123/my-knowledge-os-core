use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use cap_std::fs::Dir;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::{
    atomic::{
        AtomicWriteResult, write_new, write_replace_capability_compare_exchange_validated_at_commit,
    },
    error::MkoError,
};

const SCHEMA_VERSION_V2: u64 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationOutcomeV2 {
    Created,
    Existing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionPublicationV2 {
    pub revision: String,
    pub path: PathBuf,
    pub outcome: PublicationOutcomeV2,
}

/// Serializes a value as deterministic JSON suitable for digest inputs.
///
/// Strings and object keys are normalized to NFC with LF line endings, object
/// keys are sorted lexicographically, arrays retain their order, and the output
/// contains no insignificant whitespace.
pub fn canonical_json_bytes<T>(value: &T) -> Result<Vec<u8>, MkoError>
where
    T: Serialize,
{
    let value = serde_json::to_value(value)
        .map_err(|error| MkoError::new("canonical_json_invalid", error.to_string()))?;
    let mut output = Vec::new();
    write_canonical_json(&value, &mut output)?;
    Ok(output)
}

pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

pub fn canonical_json_sha256<T>(value: &T) -> Result<String, MkoError>
where
    T: Serialize,
{
    canonical_json_bytes(value).map(|bytes| sha256_digest(&bytes))
}

/// Publishes immutable bytes under their SHA-256 identity.
///
/// The portable filename uses `sha256-<hex>.md`; the returned logical revision
/// keeps the contract form `sha256:<hex>`.
pub fn publish_revision_v2(
    revisions_directory: &Path,
    bytes: &[u8],
) -> Result<RevisionPublicationV2, MkoError> {
    validate_real_directory(revisions_directory, "revision")?;
    let revision = sha256_digest(bytes);
    let filename = format!("{}.md", revision.replace(':', "-"));
    let path = revisions_directory.join(filename);
    let result = write_new(&path, bytes, |existing| {
        if regular_file_matches(existing, bytes)? {
            Ok(())
        } else {
            Err(MkoError::new(
                "revision_conflict",
                "the content-addressed revision path contains different bytes",
            ))
        }
    })
    .map_err(|error| map_atomic_error(error, "revision"))?;
    Ok(RevisionPublicationV2 {
        revision,
        path,
        outcome: map_outcome(result),
    })
}

/// Creates a canonical schema-v2 current pointer without replacing an existing
/// pointer. Repeating the exact same pointer is idempotent.
pub fn create_current_pointer_v2<T>(
    path: &Path,
    pointer: &T,
) -> Result<PublicationOutcomeV2, MkoError>
where
    T: Serialize,
{
    validate_parent_directory(path, "current_pointer")?;
    let bytes = canonical_pointer_bytes(pointer)?;
    let result = write_new(path, &bytes, |existing| {
        if regular_file_matches(existing, &bytes)? {
            Ok(())
        } else {
            Err(MkoError::new(
                "current_pointer_conflict",
                "the current pointer already exists with different bytes",
            ))
        }
    })
    .map_err(|error| map_atomic_error(error, "current_pointer"))?;
    Ok(map_outcome(result))
}

/// Replaces a schema-v2 current pointer only when its current bytes exactly
/// equal the canonical expected pointer bytes.
pub fn compare_and_swap_current_pointer_v2<E, R>(
    path: &Path,
    expected: &E,
    replacement: &R,
) -> Result<(), MkoError>
where
    E: Serialize,
    R: Serialize,
{
    validate_parent_directory(path, "current_pointer")?;
    let expected_bytes = canonical_pointer_bytes(expected)?;
    let replacement_bytes = canonical_pointer_bytes(replacement)?;
    let parent = path.parent().ok_or_else(|| {
        MkoError::new(
            "current_pointer_write_failed",
            "current pointer path has no parent directory",
        )
    })?;
    let filename = path.file_name().ok_or_else(|| {
        MkoError::new(
            "current_pointer_write_failed",
            "current pointer path has no filename",
        )
    })?;
    let directory = Dir::open_ambient_dir(parent, cap_std::ambient_authority())
        .map_err(|error| MkoError::new("current_pointer_write_failed", error.to_string()))?;

    write_replace_capability_compare_exchange_validated_at_commit(
        &directory,
        Path::new(filename),
        &expected_bytes,
        &replacement_bytes,
        || Ok(()),
        || Ok(()),
    )
    .map_err(|error| map_atomic_error(error, "current_pointer"))
}

fn canonical_pointer_bytes<T>(pointer: &T) -> Result<Vec<u8>, MkoError>
where
    T: Serialize,
{
    let value = serde_json::to_value(pointer)
        .map_err(|error| MkoError::new("current_pointer_invalid", error.to_string()))?;
    let Some(object) = value.as_object() else {
        return Err(MkoError::new(
            "current_pointer_invalid",
            "a schema-v2 current pointer must be a JSON object",
        ));
    };
    if object.get("schema_version").and_then(Value::as_u64) != Some(SCHEMA_VERSION_V2) {
        return Err(MkoError::new(
            "current_pointer_invalid",
            "current pointer schema_version must be 2",
        ));
    }
    canonical_json_bytes(&value)
        .map_err(|error| MkoError::new("current_pointer_invalid", error.message()))
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) -> Result<(), MkoError> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        Value::String(value) => write_json_string(&normalize_string(value), output)?,
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            let mut entries = values
                .iter()
                .map(|(key, value)| (normalize_string(key), value))
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                return Err(MkoError::new(
                    "canonical_json_invalid",
                    "object keys collide after Unicode and newline normalization",
                ));
            }
            output.push(b'{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_json_string(&key, output)?;
                output.push(b':');
                write_canonical_json(value, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn write_json_string(value: &str, output: &mut Vec<u8>) -> Result<(), MkoError> {
    let encoded = serde_json::to_string(value)
        .map_err(|error| MkoError::new("canonical_json_invalid", error.to_string()))?;
    output.extend_from_slice(encoded.as_bytes());
    Ok(())
}

fn normalize_string(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect()
}

fn validate_parent_directory(path: &Path, domain: &str) -> Result<(), MkoError> {
    let parent = path.parent().ok_or_else(|| {
        MkoError::new(
            format!("{domain}_write_failed"),
            "publication path has no parent directory",
        )
    })?;
    validate_real_directory(parent, domain)
}

fn validate_real_directory(path: &Path, domain: &str) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => Err(MkoError::new(
            format!("{domain}_destination_invalid"),
            "publication parent must be a non-symlink directory",
        )),
        Err(error) => Err(MkoError::new(
            format!("{domain}_write_failed"),
            error.to_string(),
        )),
    }
}

fn regular_file_matches(path: &Path, expected: &[u8]) -> Result<bool, MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "registry_destination_invalid",
            "publication destination is not a regular file",
        ));
    }
    let expected_len = u64::try_from(expected.len())
        .map_err(|_| MkoError::new("registry_write_failed", "expected bytes are too large"))?;
    if metadata.len() != expected_len {
        return Ok(false);
    }
    let mut actual = Vec::with_capacity(expected.len());
    File::open(path)
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?
        .take(expected_len.saturating_add(1))
        .read_to_end(&mut actual)
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    Ok(actual == expected)
}

fn map_outcome(result: AtomicWriteResult) -> PublicationOutcomeV2 {
    match result {
        AtomicWriteResult::Created => PublicationOutcomeV2::Created,
        AtomicWriteResult::Existing => PublicationOutcomeV2::Existing,
    }
}

fn map_atomic_error(error: MkoError, domain: &str) -> MkoError {
    let code = match error.code() {
        "registry_destination_invalid" => format!("{domain}_destination_invalid"),
        "registry_snapshot_changed" => format!("{domain}_snapshot_changed"),
        "registry_not_found" => format!("{domain}_not_found"),
        "registry_locked" => format!("{domain}_locked"),
        _ => return error,
    };
    MkoError::new(code, error.message())
}
