use serde::{Deserialize, Deserializer, Serialize};

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
    Retry,
    Sync,
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
    #[serde(rename = "queue")]
    Queue {
        #[serde(deserialize_with = "deserialize_schema_version")]
        schema_version: u32,
        result: JsonV2SuccessResult,
        data: QueueDataV2,
    },
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
