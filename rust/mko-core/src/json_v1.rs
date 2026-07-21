use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == 1 {
        Ok(version)
    } else {
        Err(serde::de::Error::custom("schema_version must be 1"))
    }
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum JsonV1Command {
    #[serde(rename = "add")]
    Add,
    #[serde(rename = "source.prepare")]
    SourcePrepare,
    #[serde(rename = "source.write_draft")]
    SourceWriteDraft,
    #[serde(rename = "check")]
    Check,
    #[serde(rename = "inbox")]
    Inbox,
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "doctor")]
    Doctor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessResult {
    Ok,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureResult {
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddOutcome {
    Created,
    Existing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportOutcome {
    Copied,
    AlreadyInInbox,
    ReusedInboxCopy,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserState {
    New,
    Registered,
    Incomplete,
    ReviewPending,
    Processed,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    None,
    Configure,
    Hydrate,
    Add,
    Prepare,
    WriteDraft,
    Review,
    Repair,
    Retry,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftOutcome {
    Created,
    Existing,
    Replaced,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AddData {
    pub add_outcome: AddOutcome,
    pub import_outcome: ImportOutcome,
    pub repository: String,
    pub asset_id: String,
    pub registry_path: String,
    pub provider_locator: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AddPayload {
    Single(AddData),
    Batch(BatchAddData),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchItemData {
    pub provider_locator: String,
    pub user_state: UserState,
    pub next_action: NextAction,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub asset_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub add_outcome: Option<AddOutcome>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub error: Option<JsonV1Error>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchAddData {
    pub scan_complete: bool,
    pub items: Vec<BatchItemData>,
    pub remaining: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareData {
    pub asset_id: String,
    pub source_id: String,
    pub bundle_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteDraftData {
    pub draft_outcome: DraftOutcome,
    pub source_id: String,
    pub source_path: String,
    pub content_revision: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticData {
    pub code: String,
    pub message: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckData {
    pub valid: bool,
    pub errors: Vec<DiagnosticData>,
    pub warnings: Vec<DiagnosticData>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorData {
    pub healthy: bool,
    pub checks: Vec<DoctorCheckData>,
    pub next_action: NextAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScanLimitsData {
    pub max_entries: u64,
    pub max_total_bytes: u64,
    pub max_elapsed_ms: u64,
    pub max_depth: u64,
    pub max_batch_items: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InboxItemData {
    pub provider_locator: String,
    pub user_state: UserState,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub asset_id: Option<String>,
    pub next_action: NextAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InboxData {
    pub scan_complete: bool,
    pub scan_limits: ScanLimitsData,
    pub items: Vec<InboxItemData>,
    pub errors: Vec<DiagnosticData>,
    pub warnings: Vec<DiagnosticData>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusData {
    pub healthy: bool,
    pub counts: BTreeMap<UserState, u64>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub primary_blocker: Option<DiagnosticData>,
    pub next_action: NextAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryKind {
    Configure,
    Hydrate,
    VerifyBackup,
    FixPermissions,
    ResolveHookConflict,
    Retry,
    Repair,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Recovery {
    pub kind: RecoveryKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCheckStatus {
    Healthy,
    Warning,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorCheckData {
    pub code: String,
    pub status: DoctorCheckStatus,
    pub message: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub path: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub recovery: Option<Recovery>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonV1Error {
    pub code: String,
    pub message: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub recovery: Option<Recovery>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command")]
#[serde(deny_unknown_fields)]
pub enum JsonV1Success {
    #[serde(rename = "add")]
    Add {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: SuccessResult,
        data: AddPayload,
    },
    #[serde(rename = "source.prepare")]
    SourcePrepare {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: SuccessResult,
        data: PrepareData,
    },
    #[serde(rename = "source.write_draft")]
    SourceWriteDraft {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: SuccessResult,
        data: WriteDraftData,
    },
    #[serde(rename = "check")]
    Check {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: SuccessResult,
        data: CheckData,
    },
    #[serde(rename = "inbox")]
    Inbox {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: SuccessResult,
        data: InboxData,
    },
    #[serde(rename = "status")]
    Status {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: SuccessResult,
        data: StatusData,
    },
    #[serde(rename = "doctor")]
    Doctor {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: SuccessResult,
        data: DoctorData,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonV1Failure {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub command: JsonV1Command,
    pub result: FailureResult,
    pub error: JsonV1Error,
}
