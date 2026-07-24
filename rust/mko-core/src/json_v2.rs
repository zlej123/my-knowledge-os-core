use serde::{Deserialize, Deserializer, Serialize};

use crate::review_session_v2::ReviewOpenDataV2;
use crate::setup_plan_v2::SetupPlanDataV2;

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == 2 {
        Ok(version)
    } else {
        Err(serde::de::Error::custom("schema_version must be 2"))
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
pub enum JsonV2Command {
    #[serde(rename = "capture.validate")]
    CaptureValidate,
    #[serde(rename = "capture.route")]
    CaptureRoute,
    #[serde(rename = "telegram.status")]
    TelegramStatus,
    #[serde(rename = "setup.plan")]
    SetupPlan,
    #[serde(rename = "setup.apply")]
    SetupApply,
    #[serde(rename = "add")]
    Add,
    #[serde(rename = "source.prepare")]
    SourcePrepare,
    #[serde(rename = "source.write")]
    SourceWrite,
    #[serde(rename = "knowledge.write")]
    KnowledgeWrite,
    #[serde(rename = "check")]
    Check,
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "queue")]
    Queue,
    #[serde(rename = "show")]
    Show,
    #[serde(rename = "review.open")]
    ReviewOpen,
    #[serde(rename = "review.feedback")]
    ReviewFeedback,
    #[serde(rename = "dashboard")]
    Dashboard,
    #[serde(rename = "doctor")]
    Doctor,
    #[serde(rename = "sync.status")]
    SyncStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonV2SuccessResult {
    Ok,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonV2FailureResult {
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueItemTypeV2 {
    Source,
    Knowledge,
    Combined,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueItemStateV2 {
    Unreviewed,
    Deferred,
    ChangesRequested,
    RevisedUnreviewed,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueNextActionV2 {
    Display,
    Regenerate,
    Diagnose,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueueItemV2 {
    pub item_id: String,
    pub target_ids: Vec<String>,
    pub title: String,
    pub item_type: QueueItemTypeV2,
    pub state: QueueItemStateV2,
    pub revisions: Vec<String>,
    pub next_action: QueueNextActionV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueueDataV2 {
    pub items: Vec<QueueItemV2>,
    pub scan_complete: bool,
    pub remaining: u64,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTargetStateV2 {
    Unreviewed,
    Deferred,
    ChangesRequested,
    RevisedUnreviewed,
    Approved,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShowTargetV2 {
    pub record_id: String,
    pub displayed_revision: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub review_head_id: Option<String>,
    pub state: ReviewTargetStateV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShowDataV2 {
    pub item_id: String,
    pub card_markdown: String,
    pub card_digest: String,
    pub effect_digest: String,
    pub targets: Vec<ShowTargetV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFeedbackDataV2 {
    pub review_id: String,
    pub target_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddOutcomeV2 {
    Created,
    Existing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AddSingleDataV2 {
    pub asset_id: String,
    pub outcome: AddOutcomeV2,
    pub registry_path: String,
    pub logical_locator: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AddBatchItemErrorV2 {
    pub code: String,
    pub message: String,
    pub next_action: NextActionV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AddBatchItemV2 {
    pub logical_locator: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub asset_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub outcome: Option<AddOutcomeV2>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub error: Option<AddBatchItemErrorV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AddBatchWarningV2 {
    pub code: String,
    pub message: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub logical_locator: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AddBatchDataV2 {
    pub items: Vec<AddBatchItemV2>,
    pub scan_complete: bool,
    pub remaining: u64,
    pub warnings: Vec<AddBatchWarningV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AddDataV2 {
    Single(AddSingleDataV2),
    Batch(AddBatchDataV2),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePrepareOutcomeV2 {
    Created,
    Existing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePrepareDataV2 {
    pub asset_id: String,
    pub bundle_id: String,
    pub content_digest: String,
    pub bundle_path: String,
    pub outcome: SourcePrepareOutcomeV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticWriteOutcomeV2 {
    Created,
    Existing,
    Replaced,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStateV2 {
    Current,
    RepairRequired,
    Stale,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordWriteDataV2 {
    pub record_id: String,
    pub revision: String,
    pub revision_path: String,
    pub current_path: String,
    pub outcome: SemanticWriteOutcomeV2,
    pub projection_state: ProjectionStateV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NextActionV2 {
    None,
    Configure,
    Hydrate,
    Add,
    Prepare,
    WriteSource,
    WriteKnowledge,
    Review,
    Repair,
    PreserveUserEdit,
    Retry,
    Sync,
    ConfirmRouting,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureChannelV2 {
    Telegram,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureInputTypeV2 {
    TelegramPdf,
    Youtube,
    Text,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureScopeV2 {
    General,
    Finance,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureRouteOutcomeV2 {
    ReadyGeneral,
    ReadyFinance,
    GeneralConfirmationRequired,
    FinanceConfirmationRequired,
    RoutingConfirmationRequired,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureRoutingAuthorityV2 {
    UserSelected,
    UserConfirmedProposal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureRoutingRejectionV2 {
    ConfirmationRequiresProposal,
    ConfirmationDoesNotMatchProposal,
    ConfirmationCannotResolveMixedOrConflictingProposal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramConnectionStateV2 {
    Disconnected,
    Connected,
    Incomplete,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramStatusDataV2 {
    pub profile_id: String,
    pub state: TelegramConnectionStateV2,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub bot_username: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub primary_device_id: Option<String>,
    pub next_action: NextActionV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureProposalDataV2 {
    pub proposed_scope: CaptureScopeV2,
    pub confidence: u8,
    pub mixed_subjects: bool,
    pub conflicting: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureValidateDataV2 {
    pub capture_id: String,
    pub channel: CaptureChannelV2,
    pub input_type: CaptureInputTypeV2,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub selected_scope: Option<CaptureScopeV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRouteDataV2 {
    pub capture_id: String,
    pub outcome: CaptureRouteOutcomeV2,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub confirmed_scope: Option<CaptureScopeV2>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub routing_authority: Option<CaptureRoutingAuthorityV2>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub proposal: Option<CaptureProposalDataV2>,
    pub next_action: NextActionV2,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub rejection_reason: Option<CaptureRoutingRejectionV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardCanonicalStateDataV2 {
    Ready,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardProjectionStateDataV2 {
    Current,
    RepairRequired,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardFileKindDataV2 {
    ViewDefinition,
    RecordProjection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardFileStateDataV2 {
    Current,
    Missing,
    Stale,
    UserModified,
    Unowned,
    Orphaned,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardFileDataV2 {
    pub path: String,
    pub kind: DashboardFileKindDataV2,
    pub manifest_owned: bool,
    pub state: DashboardFileStateDataV2,
    pub next_action: NextActionV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardDataV2 {
    pub canonical_state: DashboardCanonicalStateDataV2,
    pub projection_state: DashboardProjectionStateDataV2,
    pub manifest_owned_drift: bool,
    pub next_action: NextActionV2,
    pub items: Vec<DashboardFileDataV2>,
    pub scan_complete: bool,
    pub remaining: u64,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupApplyDataV2 {
    pub plan_id: String,
    pub repository_root: String,
    pub provider_root: String,
    pub profile_changed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorDetailsV2 {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonV2Error {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub next_action: NextActionV2,
    pub details: ErrorDetailsV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command")]
#[serde(deny_unknown_fields)]
pub enum JsonV2Success {
    #[serde(rename = "capture.validate")]
    CaptureValidate {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: CaptureValidateDataV2,
    },
    #[serde(rename = "capture.route")]
    CaptureRoute {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: CaptureRouteDataV2,
    },
    #[serde(rename = "telegram.status")]
    TelegramStatus {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: TelegramStatusDataV2,
    },
    #[serde(rename = "setup.plan")]
    SetupPlan {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: SetupPlanDataV2,
    },
    #[serde(rename = "setup.apply")]
    SetupApply {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: SetupApplyDataV2,
    },
    #[serde(rename = "add")]
    Add {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: AddDataV2,
    },
    #[serde(rename = "source.prepare")]
    SourcePrepare {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: SourcePrepareDataV2,
    },
    #[serde(rename = "source.write")]
    SourceWrite {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: RecordWriteDataV2,
    },
    #[serde(rename = "knowledge.write")]
    KnowledgeWrite {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: RecordWriteDataV2,
    },
    #[serde(rename = "queue")]
    Queue {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: QueueDataV2,
    },
    #[serde(rename = "show")]
    Show {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: ShowDataV2,
    },
    #[serde(rename = "review.open")]
    ReviewOpen {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: ReviewOpenDataV2,
    },
    #[serde(rename = "review.feedback")]
    ReviewFeedback {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: ReviewFeedbackDataV2,
    },
    #[serde(rename = "dashboard")]
    Dashboard {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: DashboardDataV2,
    },
}

impl JsonV2Success {
    pub fn capture_validate(data: CaptureValidateDataV2) -> Self {
        Self::CaptureValidate {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn capture_route(data: CaptureRouteDataV2) -> Self {
        Self::CaptureRoute {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn telegram_status(data: TelegramStatusDataV2) -> Self {
        Self::TelegramStatus {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn setup_plan(data: SetupPlanDataV2) -> Self {
        Self::SetupPlan {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn setup_apply(data: SetupApplyDataV2) -> Self {
        Self::SetupApply {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn add(data: AddDataV2) -> Self {
        Self::Add {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn source_prepare(data: SourcePrepareDataV2) -> Self {
        Self::SourcePrepare {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn source_write(data: RecordWriteDataV2) -> Self {
        Self::SourceWrite {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn knowledge_write(data: RecordWriteDataV2) -> Self {
        Self::KnowledgeWrite {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn queue(data: QueueDataV2) -> Self {
        Self::Queue {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn show(data: ShowDataV2) -> Self {
        Self::Show {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn review_open(data: ReviewOpenDataV2) -> Self {
        Self::ReviewOpen {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn review_feedback(data: ReviewFeedbackDataV2) -> Self {
        Self::ReviewFeedback {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }

    pub fn dashboard(data: DashboardDataV2) -> Self {
        Self::Dashboard {
            schema_version: 2,
            result: JsonV2SuccessResult::Ok,
            data,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonV2Failure {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub command: JsonV2Command,
    pub result: JsonV2FailureResult,
    pub error: JsonV2Error,
}
