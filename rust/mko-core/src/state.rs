use chrono::{DateTime, Utc};

use crate::{
    error::MkoError,
    model::{AssetRecord, AssetStatus, LastSuccessfulStep},
};

pub fn transition_allowed(from: AssetStatus, to: AssetStatus) -> bool {
    use AssetStatus::{
        Changed, Extracted, Failed, Missing, Processed, Registered, ReviewPending, Superseded,
    };

    matches!(
        (from, to),
        (Registered, Extracted | Changed | Missing | Failed)
            | (Extracted, ReviewPending | Changed | Missing | Failed)
            | (ReviewPending, Processed | Changed | Missing | Failed)
            | (Processed, Changed | Missing | Failed)
            | (
                Changed,
                Registered | Extracted | ReviewPending | Processed | Superseded | Failed
            )
            | (
                Missing,
                Registered | Extracted | ReviewPending | Processed | Failed
            )
            | (Superseded, Failed)
            | (
                Failed,
                Registered | Extracted | ReviewPending | Processed | Changed | Missing | Superseded
            )
    )
}

pub fn transition_asset(
    asset: &mut AssetRecord,
    to: AssetStatus,
    now: DateTime<Utc>,
) -> Result<(), MkoError> {
    let from = asset.asset_status.clone();
    if !transition_allowed(from.clone(), to.clone()) {
        return Err(MkoError::new(
            "invalid_state_transition",
            format!("cannot transition asset from {from:?} to {to:?}"),
        ));
    }
    let recovery_transition = matches!(
        from,
        AssetStatus::Changed | AssetStatus::Missing | AssetStatus::Failed
    ) && to != AssetStatus::Failed
        && !(from == AssetStatus::Changed && to == AssetStatus::Superseded);
    if recovery_transition && to != previous_durable_state(asset) {
        return Err(MkoError::new(
            "invalid_state_transition",
            "recovery must return the asset to its previous durable state",
        ));
    }
    asset.asset_status = to;
    match asset.asset_status {
        AssetStatus::Registered => asset.last_successful_step = LastSuccessfulStep::Registered,
        AssetStatus::Extracted => asset.last_successful_step = LastSuccessfulStep::Extracted,
        AssetStatus::ReviewPending => asset.last_successful_step = LastSuccessfulStep::Drafted,
        AssetStatus::Processed => asset.last_successful_step = LastSuccessfulStep::Reviewed,
        AssetStatus::Changed
        | AssetStatus::Missing
        | AssetStatus::Superseded
        | AssetStatus::Failed => {}
    }
    asset.updated_at = now;
    Ok(())
}

pub fn previous_durable_state(asset: &AssetRecord) -> AssetStatus {
    match asset.last_successful_step {
        LastSuccessfulStep::Registered => AssetStatus::Registered,
        LastSuccessfulStep::Extracted => AssetStatus::Extracted,
        LastSuccessfulStep::Drafted => AssetStatus::ReviewPending,
        LastSuccessfulStep::Reviewed => AssetStatus::Processed,
    }
}
