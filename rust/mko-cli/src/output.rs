use std::io::{self, Write};

use mko_core::{
    error::MkoError,
    json_v1::{FailureResult, JsonV1Command, JsonV1Error, JsonV1Failure, JsonV1Success, Recovery},
};

pub use mko_core::json_v1::RecoveryKind;

pub fn emit_encoded_json(encoded: &str) -> Result<(), MkoError> {
    let stdout = io::stdout();
    write_json_line(&mut stdout.lock(), encoded)
}

pub fn emit_json_value(value: &serde_json::Value) -> Result<(), MkoError> {
    let stdout = io::stdout();
    emit_json_value_to(&mut stdout.lock(), value)
}

fn emit_json_value_to(writer: &mut impl Write, value: &serde_json::Value) -> Result<(), MkoError> {
    let encoded = serde_json::to_string(value)
        .map_err(|error| MkoError::new("json_output_failed", error.to_string()))?;
    write_json_line(writer, &encoded)
}

pub fn emit_legacy_json_error(code: &str, message: &str) -> Result<(), MkoError> {
    emit_json_value(&serde_json::json!({
        "result": "error",
        "error": {"code": code, "message": message},
    }))
}

pub fn emit_json_v1(output: JsonV1Success) -> Result<(), MkoError> {
    let encoded = serde_json::to_string(&output)
        .map_err(|error| MkoError::new("json_output_failed", error.to_string()))?;
    emit_encoded_json(&encoded)
}

fn write_json_line(writer: &mut impl Write, encoded: &str) -> Result<(), MkoError> {
    writeln!(writer, "{encoded}")
        .and_then(|()| writer.flush())
        .map_err(|error| MkoError::new("json_output_failed", error.to_string()))
}

pub fn emit_json_v1_failure(command: JsonV1Command, error: &MkoError) -> Result<(), MkoError> {
    let failure = json_v1_failure(command, error);
    let encoded = serde_json::to_string(&failure)
        .map_err(|error| MkoError::new("json_output_failed", error.to_string()))?;
    emit_encoded_json(&encoded)
}

pub fn json_v1_failure(command: JsonV1Command, error: &MkoError) -> JsonV1Failure {
    JsonV1Failure {
        schema_version: 1,
        command,
        result: FailureResult::Error,
        error: JsonV1Error {
            code: error.code().into(),
            message: error.message().into(),
            recovery: recovery_for_error_code(error.code()).map(|kind| Recovery { kind }),
        },
    }
}

pub fn recovery_for_error_code(code: &str) -> Option<RecoveryKind> {
    match code {
        "profile_missing"
        | "context_not_found"
        | "profile_invalid"
        | "inbox_unavailable"
        | "repository_not_configured" => Some(RecoveryKind::Configure),
        "provider_hydration_failed" | "provider_not_hydrated" => Some(RecoveryKind::Hydrate),
        "backup_confirmation_required" => Some(RecoveryKind::VerifyBackup),
        "profile_permissions_invalid" | "provider_permissions_invalid" | "permission_denied" => {
            Some(RecoveryKind::FixPermissions)
        }
        "hook_conflict" | "hook_path_conflict" => Some(RecoveryKind::ResolveHookConflict),
        "extraction_timeout"
        | "provider_scan_incomplete"
        | "provider_import_locked"
        | "registry_locked"
        | "lock_held" => Some(RecoveryKind::Retry),
        "registry_provider_missing"
        | "registry_provider_mismatch"
        | "source_state_mismatch"
        | "lineage_repair_needed"
        | "repository_state_inconsistent" => Some(RecoveryKind::Repair),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use serde_json::json;

    use super::{emit_json_value_to, write_json_line};

    struct FlushFailure;

    impl Write for FlushFailure {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed pipe"))
        }
    }

    #[test]
    fn json_output_write_failures_are_mko_errors() {
        let error = write_json_line(&mut FlushFailure, "{}")
            .expect_err("a stdout flush failure must not panic");

        assert_eq!(error.code(), "json_output_failed");
        assert_eq!(error.message(), "closed pipe");
    }

    #[test]
    fn json_value_output_preserves_the_serialized_legacy_bytes() {
        let mut output = Vec::new();

        emit_json_value_to(
            &mut output,
            &json!({"result": "error", "error": {"code": "usage", "message": "bad input"}}),
        )
        .unwrap();

        assert_eq!(
            output,
            b"{\"error\":{\"code\":\"usage\",\"message\":\"bad input\"},\"result\":\"error\"}\n"
        );
    }
}
