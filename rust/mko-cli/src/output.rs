use std::io::{self, Write};

use mko_core::{
    error::MkoError,
    json_v1::{FailureResult, JsonV1Command, JsonV1Error, JsonV1Failure, JsonV1Success, Recovery},
    json_v2::{
        ErrorDetailsV2, JsonV2Command, JsonV2Error, JsonV2Failure, JsonV2FailureResult,
        JsonV2Success, NextActionV2,
    },
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

pub fn emit_json_v2(output: JsonV2Success) -> Result<(), MkoError> {
    let encoded = serde_json::to_string(&output)
        .map_err(|error| MkoError::new("json_output_failed", error.to_string()))?;
    emit_encoded_json(&encoded)
}

pub fn emit_json_v2_failure(command: JsonV2Command, error: &MkoError) -> Result<(), MkoError> {
    let failure = JsonV2Failure {
        schema_version: 2,
        command,
        result: JsonV2FailureResult::Error,
        error: JsonV2Error {
            code: error.code().into(),
            message: error.message().into(),
            retryable: matches!(
                error.code(),
                "lock_held"
                    | "repository_lock_held"
                    | "review_session_random_failed"
                    | "setup_profile_locked"
            ),
            next_action: json_v2_next_action(error.code()),
            details: ErrorDetailsV2::default(),
        },
    };
    let encoded = serde_json::to_string(&failure)
        .map_err(|error| MkoError::new("json_output_failed", error.to_string()))?;
    emit_encoded_json(&encoded)
}

pub(crate) fn json_v2_next_action(code: &str) -> NextActionV2 {
    match code {
        "kb_config_unreadable"
        | "kb_schema_unsupported"
        | "context_not_found"
        | "setup_plan_not_found"
        | "setup_plan_expired"
        | "setup_plan_consumed"
        | "setup_plan_stale" => NextActionV2::Configure,
        "provider_hydration_required"
        | "provider_not_hydrated"
        | "asset_not_hydrated"
        | "hydration_confirmation_required" => NextActionV2::Hydrate,
        // A page that produced no text, or more text than may be stored, is not
        // retried by fetching it again the same way: the answer is to bring
        // different material.
        "asset_outside_inbox"
        | "asset_path_required"
        | "pdf_text_unreadable"
        | "snapshot_text_empty"
        | "snapshot_too_large"
        | "snapshot_unreadable"
        | "snapshot_arguments_incomplete"
        | "snapshot_timestamp_invalid" => NextActionV2::Add,
        // Registering the same page again rewrites content-addressed evidence
        // that no longer matches its identity, so this is repairable by the
        // caller rather than a dead end.
        "registered_asset_changed" | "snapshot_damaged" => NextActionV2::Add,
        // Write-path failures on the append-only stores: the observation was
        // not recorded, and the same call is the way to record it.
        "snapshot_write_failed"
        | "snapshot_destination_invalid"
        | "question_write_failed"
        | "question_destination_invalid"
        | "question_unreadable"
        | "attempt_write_failed"
        | "attempt_destination_invalid"
        | "attempt_unreadable" => NextActionV2::Retry,
        "question_invalid" | "asset_binding_invalid" => NextActionV2::Add,
        "projection_not_found"
        | "projection_snapshot_changed"
        | "dashboard_drift"
        | "dashboard_snapshot_changed"
        | "review_target_blocked" => NextActionV2::Repair,
        "dashboard_user_modified"
        | "dashboard_projection_user_modified"
        | "dashboard_orphan_projection" => NextActionV2::PreserveUserEdit,
        "lock_held"
        | "repository_lock_held"
        | "review_session_random_failed"
        | "setup_profile_locked" => NextActionV2::Retry,
        "repository_lock_stale" => NextActionV2::Repair,
        "review_session_expired"
        | "review_session_consumed"
        | "review_snapshot_stale"
        | "record_revision_stale"
        | "replacement_revision_required" => NextActionV2::Review,
        "skill_version_mismatch" | "schema_not_found" => NextActionV2::Reinstall,
        _ => NextActionV2::None,
    }
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
    let message = json_v1_failure_message(&command, error.code());
    JsonV1Failure {
        schema_version: 1,
        command,
        result: FailureResult::Error,
        error: JsonV1Error {
            code: error.code().into(),
            message: message.into(),
            recovery: recovery_for_error_code(error.code()).map(|kind| Recovery { kind }),
        },
    }
}

fn json_v1_failure_message(command: &JsonV1Command, code: &str) -> &'static str {
    match command {
        JsonV1Command::Add => match code {
            "provider_not_found" => "The provider item was not found.",
            "inbox_unavailable" => "The inbox could not be scanned.",
            "backup_confirmation_required" => {
                "confirm a verified second copy before registering an only-copy or temporary PDF"
            }
            _ => "The PDF could not be added.",
        },
        JsonV1Command::SourcePrepare => match code {
            "asset_incomplete" => "The asset is not ready for preparation.",
            _ => "The source could not be prepared.",
        },
        JsonV1Command::SourceWriteDraft => match code {
            "draft_conflict" => "A pending draft already exists.",
            _ => "The source draft could not be written.",
        },
        JsonV1Command::Check => "The repository could not be checked.",
        JsonV1Command::Doctor => "The configuration could not be inspected.",
        JsonV1Command::Inbox => match code {
            "inbox_unavailable" => "The inbox is not configured.",
            _ => "The inbox could not be inspected.",
        },
        JsonV1Command::Status => match code {
            "repository_not_configured" => "No default repository is configured.",
            _ => "The repository status could not be inspected.",
        },
        JsonV1Command::KnowledgeWrite => match code {
            "asset_not_found" => "The asset was not found.",
            "replace_required" => "Regenerating this knowledge note requires --replace.",
            _ => "The knowledge note could not be written.",
        },
        JsonV1Command::KnowledgeReview => match code {
            "human_confirmation_required" => "Knowledge review requires an interactive terminal.",
            "knowledge_not_found" => "No unreviewed knowledge note is available for review.",
            _ => "The knowledge note could not be reviewed.",
        },
        JsonV1Command::KnowledgeSearch => "The knowledge base could not be searched.",
        JsonV1Command::KnowledgeShow => match code {
            "knowledge_not_found" => "No knowledge note was found for that asset.",
            _ => "The knowledge note could not be shown.",
        },
        JsonV1Command::KnowledgeList => "The knowledge notes could not be listed.",
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
