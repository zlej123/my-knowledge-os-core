use std::collections::BTreeMap;

use crate::{
    inbox::InboxScanResult,
    json_v1::{DiagnosticData, NextAction, StatusData, UserState},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusReport {
    pub healthy: bool,
    pub counts: BTreeMap<UserState, u64>,
    pub primary_blocker: Option<DiagnosticData>,
    pub next_action: NextAction,
}

pub fn status_from_inbox(inbox: &InboxScanResult) -> StatusReport {
    let counts = inbox.state_counts.clone();
    let (primary_blocker, next_action) = select_status_decision(
        inbox.scan_complete,
        &inbox.items,
        &inbox.errors,
        &inbox.warnings,
    );
    let healthy =
        counts[&UserState::Blocked] == 0 && inbox.errors.is_empty() && inbox.scan_complete;
    StatusReport {
        healthy,
        counts,
        primary_blocker,
        next_action,
    }
}

pub(crate) fn select_status_decision(
    scan_complete: bool,
    items: &[crate::catalog::CatalogItem],
    errors: &[DiagnosticData],
    warnings: &[DiagnosticData],
) -> (Option<DiagnosticData>, NextAction) {
    let mut blockers = errors
        .iter()
        .cloned()
        .map(|diagnostic| (action_for_diagnostic(&diagnostic), 0_u8, diagnostic))
        .chain(items.iter().filter_map(|item| {
            (item.user_state == UserState::Blocked)
                .then(|| {
                    item.diagnostic
                        .clone()
                        .map(|diagnostic| (item.next_action.clone(), 1_u8, diagnostic))
                })
                .flatten()
        }))
        .chain(
            (!scan_complete)
                .then(|| {
                    warnings
                        .iter()
                        .cloned()
                        .map(|diagnostic| (NextAction::Retry, 2_u8, diagnostic))
                })
                .into_iter()
                .flatten(),
        )
        .collect::<Vec<_>>();
    blockers.sort_by(
        |(left_action, left_source, left), (right_action, right_source, right)| {
            action_priority(left_action)
                .cmp(&action_priority(right_action))
                .then(left_source.cmp(right_source))
                .then(left.code.cmp(&right.code))
                .then(left.path.cmp(&right.path))
        },
    );
    if let Some((action, _, diagnostic)) = blockers.into_iter().next() {
        return (Some(diagnostic), action);
    }
    let action = items
        .iter()
        .map(|item| item.next_action.clone())
        .min_by_key(action_priority)
        .unwrap_or(NextAction::None);
    (None, action)
}

fn action_for_diagnostic(diagnostic: &DiagnosticData) -> NextAction {
    match diagnostic.code.as_str() {
        "configuration_missing" | "configuration_invalid" => NextAction::Configure,
        "provider_missing" => NextAction::Hydrate,
        "lock_active" | "lock_scan_incomplete" => NextAction::Retry,
        _ => NextAction::Repair,
    }
}

fn action_priority(action: &NextAction) -> u8 {
    match action {
        NextAction::Configure => 0,
        NextAction::Repair => 1,
        NextAction::Hydrate => 2,
        NextAction::Retry => 3,
        NextAction::Review => 4,
        NextAction::WriteDraft => 5,
        NextAction::Prepare => 6,
        NextAction::Add => 7,
        NextAction::None => 8,
    }
}

impl From<StatusReport> for StatusData {
    fn from(report: StatusReport) -> Self {
        Self {
            healthy: report.healthy,
            counts: report.counts,
            primary_blocker: report.primary_blocker,
            next_action: report.next_action,
        }
    }
}
