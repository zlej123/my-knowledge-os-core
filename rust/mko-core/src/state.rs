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
    if recovery_transition && previous_durable_state(asset) != Some(to.clone()) {
        return Err(MkoError::new(
            "invalid_state_transition",
            "recovery must return the asset to its previous durable state",
        ));
    }
    if recovery_transition {
        asset.durable_state_history.pop();
    } else if matches!(
        to,
        AssetStatus::Changed | AssetStatus::Missing | AssetStatus::Failed
    ) {
        asset.durable_state_history.push(from.clone());
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

pub fn previous_durable_state(asset: &AssetRecord) -> Option<AssetStatus> {
    asset.durable_state_history.last().cloned()
}

pub fn validate_asset_state(asset: &AssetRecord) -> Result<(), MkoError> {
    use AssetStatus::{
        Changed, Extracted, Failed, Missing, Processed, Registered, ReviewPending, Superseded,
    };

    let history = &asset.durable_state_history;
    if matches!(
        asset.asset_status,
        Registered | Extracted | ReviewPending | Processed
    ) && !history.is_empty()
    {
        return Err(invalid_history(
            "stable Asset state retains a recovery checkpoint",
        ));
    }
    if matches!(asset.asset_status, Changed | Missing | Failed | Superseded) {
        let Some(first) = history.first() else {
            return Err(invalid_history(
                "recoverable Asset state has no durable checkpoint",
            ));
        };
        if !matches!(first, Registered | Extracted | ReviewPending | Processed) {
            return Err(invalid_history(
                "recovery history must begin with a stable state",
            ));
        }
        if history.len() > 2 {
            return Err(invalid_history(
                "recovery history is deeper than the v0.1 state model",
            ));
        }
        for pair in history.windows(2) {
            if !matches!(pair, [Changed | Missing | Superseded, Failed]) {
                return Err(invalid_history(
                    "recovery checkpoints contain an invalid nesting",
                ));
            }
        }
        let previous = history.last().expect("non-empty checked above");
        let current_follows_checkpoint = if asset.asset_status == Superseded {
            history.len() == 1
        } else {
            transition_allowed(previous.clone(), asset.asset_status.clone())
        };
        if !current_follows_checkpoint {
            return Err(invalid_history(
                "current recoverable state cannot follow its checkpoint",
            ));
        }
    }

    let stable = history.first().unwrap_or(&asset.asset_status);
    let expected_step = match stable {
        Registered => Some(LastSuccessfulStep::Registered),
        Extracted => Some(LastSuccessfulStep::Extracted),
        ReviewPending => Some(LastSuccessfulStep::Drafted),
        Processed => Some(LastSuccessfulStep::Reviewed),
        Changed | Missing | Failed | Superseded => None,
    };
    if expected_step.is_some_and(|expected| expected != asset.last_successful_step) {
        return Err(invalid_history(
            "Asset state disagrees with its last successful step",
        ));
    }
    Ok(())
}

fn invalid_history(message: &str) -> MkoError {
    MkoError::new("invalid_state_transition", message)
}
