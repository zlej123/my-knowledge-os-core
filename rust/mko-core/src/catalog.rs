use crate::{
    json_v1::{DiagnosticData, NextAction, UserState},
    model::AssetStatus,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceObservation {
    Absent,
    ReviewPending,
    Approved,
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogBlocker {
    None,
    ProviderMissing,
    ProviderChanged,
    ActiveLock,
    StateMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogEvidence {
    pub asset_status: Option<AssetStatus>,
    pub source: SourceObservation,
    pub blocker: CatalogBlocker,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogItem {
    pub provider_locator: String,
    pub user_state: UserState,
    pub asset_id: Option<String>,
    pub next_action: NextAction,
    pub diagnostic: Option<DiagnosticData>,
}

pub fn classify_catalog_item(
    provider_locator: impl Into<String>,
    asset_id: Option<String>,
    evidence: CatalogEvidence,
) -> CatalogItem {
    let provider_locator = provider_locator.into();
    let (user_state, next_action, diagnostic) = classify(&provider_locator, &evidence);
    CatalogItem {
        provider_locator,
        user_state,
        asset_id,
        next_action,
        diagnostic,
    }
}

fn classify(
    provider_locator: &str,
    evidence: &CatalogEvidence,
) -> (UserState, NextAction, Option<DiagnosticData>) {
    let blocked = match evidence.blocker {
        CatalogBlocker::None => None,
        CatalogBlocker::ProviderMissing => Some((
            NextAction::Hydrate,
            "provider_missing",
            "The registered PDF is not readable in the inbox.",
        )),
        CatalogBlocker::ProviderChanged => Some((
            NextAction::Repair,
            "registry_provider_mismatch",
            "The registered PDF no longer matches its Registry record.",
        )),
        CatalogBlocker::ActiveLock => Some((
            NextAction::Retry,
            "lock_active",
            "Another operation is using this item.",
        )),
        CatalogBlocker::StateMismatch => Some((
            NextAction::Repair,
            "source_state_mismatch",
            "The Asset and Source states are inconsistent.",
        )),
    };
    if let Some((action, code, message)) = blocked {
        return (
            UserState::Blocked,
            action,
            Some(diagnostic(code, message, provider_locator)),
        );
    }

    match evidence.asset_status.as_ref() {
        None => (UserState::New, NextAction::Add, None),
        Some(AssetStatus::Registered) => (UserState::Registered, NextAction::Prepare, None),
        Some(AssetStatus::Extracted) => (UserState::Incomplete, NextAction::WriteDraft, None),
        Some(AssetStatus::ReviewPending) if evidence.source == SourceObservation::ReviewPending => {
            (UserState::ReviewPending, NextAction::Review, None)
        }
        Some(AssetStatus::Processed) if evidence.source == SourceObservation::Approved => {
            (UserState::Processed, NextAction::None, None)
        }
        Some(AssetStatus::Missing) => (
            UserState::Blocked,
            NextAction::Hydrate,
            Some(diagnostic(
                "provider_missing",
                "The registered PDF is not readable in the inbox.",
                provider_locator,
            )),
        ),
        Some(AssetStatus::Failed) => (
            UserState::Blocked,
            NextAction::Retry,
            Some(diagnostic(
                "asset_failed",
                "The previous processing attempt failed.",
                provider_locator,
            )),
        ),
        Some(AssetStatus::Changed) | Some(AssetStatus::Superseded) => (
            UserState::Blocked,
            NextAction::Repair,
            Some(diagnostic(
                "registry_provider_mismatch",
                "The Registry lineage requires repair.",
                provider_locator,
            )),
        ),
        Some(_) => (
            UserState::Blocked,
            NextAction::Repair,
            Some(diagnostic(
                "source_state_mismatch",
                "The Asset and Source states are inconsistent.",
                provider_locator,
            )),
        ),
    }
}

fn diagnostic(code: &str, message: &str, path: &str) -> DiagnosticData {
    DiagnosticData {
        code: code.into(),
        message: message.into(),
        path: Some(path.into()),
    }
}
