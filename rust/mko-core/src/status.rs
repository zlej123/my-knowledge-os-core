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
    let primary_blocker = inbox
        .errors
        .first()
        .cloned()
        .or_else(|| inbox.primary_blocker.clone())
        .or_else(|| {
            (!inbox.scan_complete)
                .then(|| inbox.warnings.first().cloned())
                .flatten()
        });
    let healthy =
        counts[&UserState::Blocked] == 0 && inbox.errors.is_empty() && inbox.scan_complete;
    let next_action = if !inbox.scan_complete && counts[&UserState::Blocked] == 0 {
        NextAction::Retry
    } else {
        inbox.recommended_action.clone()
    };
    StatusReport {
        healthy,
        counts,
        primary_blocker,
        next_action,
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
