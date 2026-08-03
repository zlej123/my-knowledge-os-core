use std::path::Path;

use crate::{
    asset_v2::inspect_inbox_pdf_assets_v2,
    config::KnowledgeConfig,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    inbox::{InboxScanRequest, scan_inbox},
    json_v1::UserState,
    provider_scan::ElapsedClock,
    queue_v2::summarize_home_queue_v2,
    status::status_from_inbox,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryGeneration {
    LegacyV1,
    V3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeNextAction {
    Add,
    Review,
    Repair,
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyHomeReport {
    pub new_material: u64,
    pub registered: u64,
    pub incomplete: u64,
    pub review_pending: u64,
    pub complete: u64,
    pub blocked: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V3HomeReport {
    pub new_material: u64,
    pub registered: u64,
    /// Registered Assets that have not become a Source or Knowledge record yet.
    ///
    /// Extraction can fail, and a session can end mid-way; without this the
    /// material is registered, invisible, and nobody is waiting on it.
    pub in_progress: u64,
    pub review_pending: u64,
    pub changes_requested: u64,
    pub approved_knowledge: u64,
    pub blocked: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HomeReport {
    Legacy(LegacyHomeReport),
    V3(V3HomeReport),
}

impl HomeReport {
    pub fn generation(&self) -> RepositoryGeneration {
        match self {
            Self::Legacy(_) => RepositoryGeneration::LegacyV1,
            Self::V3(_) => RepositoryGeneration::V3,
        }
    }

    pub fn next_action(&self) -> HomeNextAction {
        match self {
            Self::Legacy(report) => {
                if report.blocked > 0 {
                    HomeNextAction::Repair
                } else if report.review_pending > 0 {
                    HomeNextAction::Review
                } else if report.new_material > 0 || report.registered > 0 || report.incomplete > 0
                {
                    HomeNextAction::Add
                } else {
                    HomeNextAction::None
                }
            }
            Self::V3(report) => {
                if report.blocked > 0 {
                    HomeNextAction::Repair
                } else if report.review_pending > 0 || report.changes_requested > 0 {
                    HomeNextAction::Review
                } else if report.new_material > 0 || report.in_progress > 0 {
                    HomeNextAction::Add
                } else {
                    HomeNextAction::None
                }
            }
        }
    }
}

pub fn detect_repository_generation(
    repository_root: &Path,
) -> Result<RepositoryGeneration, MkoError> {
    match KnowledgeConfigV2::read(repository_root) {
        Ok(_) => Ok(RepositoryGeneration::V3),
        Err(v3_error) => {
            if KnowledgeConfig::read(repository_root).is_ok() {
                Ok(RepositoryGeneration::LegacyV1)
            } else {
                Err(v3_error)
            }
        }
    }
}

pub fn inspect_home(
    repository_root: &Path,
    provider_root: &Path,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<HomeReport, MkoError> {
    match detect_repository_generation(repository_root)? {
        RepositoryGeneration::LegacyV1 => {
            let inbox = scan_inbox(
                InboxScanRequest::new(repository_root, provider_root),
                elapsed_clock,
            )?;
            let status = status_from_inbox(&inbox);
            let count = |state| status.counts.get(&state).copied().unwrap_or_default();
            Ok(HomeReport::Legacy(LegacyHomeReport {
                new_material: count(UserState::New),
                registered: count(UserState::Registered),
                incomplete: count(UserState::Incomplete),
                review_pending: count(UserState::ReviewPending),
                complete: count(UserState::Processed),
                blocked: count(UserState::Blocked),
            }))
        }
        RepositoryGeneration::V3 => {
            let inbox = inspect_inbox_pdf_assets_v2(repository_root, provider_root, elapsed_clock)?;
            let queue = summarize_home_queue_v2(repository_root)?;
            let in_progress = inbox
                .registered_asset_ids
                .iter()
                .filter(|id| !queue.recorded_asset_ids.contains(*id))
                .count() as u64;
            Ok(HomeReport::V3(V3HomeReport {
                new_material: inbox.new_count,
                registered: inbox.registered_count,
                in_progress,
                review_pending: queue.review_pending,
                changes_requested: queue.changes_requested,
                approved_knowledge: queue.approved_knowledge,
                blocked: inbox.blocked_count.saturating_add(queue.blocked),
            }))
        }
    }
}
