use std::{
    fs,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use mko_core::{
    asset_v2::{HydrationConfirmationV2, read_asset_v2},
    clock::{Clock, SystemClock},
    config::KnowledgeConfig,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    json_v2::{
        JsonV2Success, ProjectionStateV2, QueueItemStateV2, QueueItemTypeV2, QueueNextActionV2,
        RecordWriteDataV2, ReviewFeedbackDataV2, ReviewTargetStateV2, SemanticWriteOutcomeV2,
        ShowDataV2, ShowTargetV2, SourcePrepareDataV2, SourcePrepareOutcomeV2,
    },
    model_v2::{KnowledgeResponseV2, PreparedMetadataV2, SourceResponseV2},
    prepared_v2::{
        PreparePdfAssetRequestV2, PreparedPdfResultV2, PreparedPersistenceOutcomeV2,
        cleanup_prepared_sessions_v2, prepare_pdf_asset_v2, prepare_snapshot_asset_v2,
        read_prepared_content_v2,
    },
    queue_v2::{ReviewCardTargetStateV2, derive_queue_v2, show_review_card_v2},
    records_v2::{
        AssetOriginV2, RecordProjectionStatusV2, RecordWriteOutcomeV2,
        WriteKnowledgeRecordRequestV2, WriteSourceRecordRequestV2, write_knowledge_record_v2,
        write_source_record_v2,
    },
    review_session_v2::{
        ReviewSessionDecisionInputV2, apply_review_session_decision_v2, open_review_session_v2,
    },
    review_v2::{TtyReviewOutcomeV2, publish_tty_review_v2},
};
use serde::de::DeserializeOwned;

use crate::output::emit_json_v2;

const MAX_REVIEW_INPUT_BYTES: u64 = 512 * 1024;
const MAX_SEMANTIC_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

pub fn is_v2_repository(repository: &Path) -> Result<bool, MkoError> {
    let marker = repository.join("knowledge-os.yaml");
    match KnowledgeConfigV2::read(repository) {
        Ok(_) => {
            cleanup_prepared_sessions_v2(repository, &SystemClock)?;
            Ok(true)
        }
        Err(v2_error) => {
            if KnowledgeConfig::read(repository).is_ok() {
                return Ok(false);
            }
            match fs::symlink_metadata(marker) {
                Err(io_error) if io_error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(io_error) => Err(MkoError::new("kb_config_unreadable", io_error.to_string())),
                Ok(_) => Err(v2_error),
            }
        }
    }
}

pub fn queue(repository: &Path) -> Result<(), MkoError> {
    cleanup_prepared_sessions_v2(repository, &SystemClock)?;
    let queue = derive_queue_v2(repository)?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if queue.items.is_empty() {
        writeln!(output, "검토 대기 항목이 없습니다.")
            .map_err(|error| output_error(error.to_string()))?;
    } else {
        writeln!(output, "검토 대기열 ({}개)", queue.items.len())
            .map_err(|error| output_error(error.to_string()))?;
        for (index, item) in queue.items.iter().enumerate() {
            writeln!(
                output,
                "{}. {} [{} / {}]",
                index + 1,
                item.title,
                item_type_label(&item.item_type),
                state_label(&item.state),
            )
            .and_then(|()| writeln!(output, "   ID: {}", item.item_id))
            .and_then(|()| writeln!(output, "   다음 작업: {}", action_label(&item.next_action)))
            .map_err(|error| output_error(error.to_string()))?;
        }
    }
    if !queue.scan_complete || queue.remaining != 0 {
        writeln!(
            output,
            "주의: 검색이 완전하지 않습니다 (남은 항목: {}).",
            queue.remaining
        )
        .map_err(|error| output_error(error.to_string()))?;
    }
    output
        .flush()
        .map_err(|error| output_error(error.to_string()))
}

pub fn queue_json_v2(repository: &Path) -> Result<(), MkoError> {
    cleanup_prepared_sessions_v2(repository, &SystemClock)?;
    emit_json_v2(JsonV2Success::queue(derive_queue_v2(repository)?))
}

pub fn show(repository: &Path, stable_id: &str) -> Result<(), MkoError> {
    cleanup_prepared_sessions_v2(repository, &SystemClock)?;
    let card = show_review_card_v2(repository, stable_id)?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    output
        .write_all(&card.card_bytes)
        .and_then(|()| output.flush())
        .map_err(|error| output_error(error.to_string()))
}

pub fn show_json_v2(repository: &Path, stable_id: &str) -> Result<(), MkoError> {
    cleanup_prepared_sessions_v2(repository, &SystemClock)?;
    let card = show_review_card_v2(repository, stable_id)?;
    let card_markdown = String::from_utf8(card.card_bytes)
        .map_err(|error| MkoError::new("review_card_invalid", error.to_string()))?;
    let targets = card
        .targets
        .into_iter()
        .map(|target| ShowTargetV2 {
            record_id: target.snapshot.record_id,
            displayed_revision: target.snapshot.displayed_revision,
            review_head_id: target.snapshot.expected_review_head_id,
            state: json_target_state(target.state),
            current_feedback: target.current_feedback,
            addressed_feedback: target.addressed_feedback,
            previous_reviewed_revision: target.previous_reviewed_revision,
        })
        .collect();
    emit_json_v2(JsonV2Success::show(ShowDataV2 {
        item_id: card.item_id,
        asset_id: card.asset_id,
        card_markdown,
        card_digest: card.card_digest,
        effect_digest: card.effect_digest,
        targets,
    }))
}

pub fn review_open_json_v2(
    repository: &Path,
    stable_id: &str,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    emit_json_v2(JsonV2Success::review_open(open_review_session_v2(
        repository, stable_id, clock,
    )?))
}

pub fn review_feedback_json_v2(
    repository: &Path,
    input_path: &Path,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let publication =
        apply_review_session_decision_v2(repository, read_decision_input(input_path)?, clock)?;
    emit_json_v2(JsonV2Success::review_feedback(ReviewFeedbackDataV2 {
        review_id: publication.record.id,
        target_ids: publication
            .record
            .targets
            .into_iter()
            .map(|target| target.record_id)
            .collect(),
    }))
}

pub fn prepare_source_json_v2(
    repository: &Path,
    provider: &Path,
    asset_id: &str,
    confirm_download: bool,
    worker_executable: &Path,
) -> Result<(), MkoError> {
    // A snapshot has no provider file to inspect, fingerprint, or extract from:
    // its text is already in the knowledge base and its hash is its identity.
    // Routing on the origin is what makes "give me this link" reach the same
    // drafting flow as a PDF.
    let metadata = PreparedMetadataV2 {
        title: None,
        authors: Vec::new(),
        created_at: None,
    };
    if read_asset_v2(repository, asset_id)?.origin == AssetOriginV2::WebSnapshot {
        let result = prepare_snapshot_asset_v2(repository, asset_id, metadata)?;
        return emit_prepared_session_v2(result);
    }
    let result = prepare_pdf_asset_v2(
        PreparePdfAssetRequestV2 {
            repository_root: repository,
            provider_root: provider,
            asset_id,
            metadata: PreparedMetadataV2 {
                title: None,
                authors: Vec::new(),
                created_at: None,
            },
            hydration_confirmation: if confirm_download {
                HydrationConfirmationV2::Confirmed
            } else {
                HydrationConfirmationV2::NotConfirmed
            },
        },
        worker_executable,
    )?;
    emit_prepared_session_v2(result)
}

/// One envelope for both origins: what a caller does next with a prepared
/// bundle does not depend on what it was prepared from.
fn emit_prepared_session_v2(result: PreparedPdfResultV2) -> Result<(), MkoError> {
    emit_json_v2(JsonV2Success::source_prepare(SourcePrepareDataV2 {
        asset_id: result.bundle.asset_id,
        bundle_id: result.bundle.bundle_id,
        content_digest: result.bundle.content_digest,
        bundle_path: result.bundle_path.display().to_string(),
        outcome: match result.outcome {
            PreparedPersistenceOutcomeV2::Created => SourcePrepareOutcomeV2::Created,
            PreparedPersistenceOutcomeV2::Existing => SourcePrepareOutcomeV2::Existing,
        },
    }))
}

pub fn write_source_json_v2(
    repository: &Path,
    bundle_path: &Path,
    response_path: &Path,
    expected_revision: Option<&str>,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let bundle = read_prepared_content_v2(bundle_path)?;
    let asset = read_asset_v2(repository, &bundle.asset_id)?;
    let response = read_json_input::<SourceResponseV2>(
        response_path,
        "source_response_invalid",
        MAX_SEMANTIC_RESPONSE_BYTES,
    )?;
    let result = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: repository,
            asset: &asset,
            bundle: &bundle,
            response: &response,
            expected_revision,
        },
        clock,
    )?;
    emit_json_v2(JsonV2Success::source_write(record_write_data(result)))
}

pub fn write_knowledge_json_v2(
    repository: &Path,
    bundle_path: &Path,
    response_path: &Path,
    expected_revision: Option<&str>,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let bundle = read_prepared_content_v2(bundle_path)?;
    let asset = read_asset_v2(repository, &bundle.asset_id)?;
    let response = read_json_input::<KnowledgeResponseV2>(
        response_path,
        "knowledge_response_invalid",
        MAX_SEMANTIC_RESPONSE_BYTES,
    )?;
    let result = write_knowledge_record_v2(
        WriteKnowledgeRecordRequestV2 {
            repository_root: repository,
            asset: &asset,
            bundle: &bundle,
            response: &response,
            expected_revision,
        },
        clock,
    )?;
    emit_json_v2(JsonV2Success::knowledge_write(record_write_data(result)))
}

fn record_write_data(result: mko_core::records_v2::RecordWriteResultV2) -> RecordWriteDataV2 {
    RecordWriteDataV2 {
        record_id: result.record_id,
        revision: result.revision,
        revision_path: result.revision_path.display().to_string(),
        current_path: result.current_path.display().to_string(),
        outcome: match result.outcome {
            RecordWriteOutcomeV2::Created => SemanticWriteOutcomeV2::Created,
            RecordWriteOutcomeV2::Existing => SemanticWriteOutcomeV2::Existing,
            RecordWriteOutcomeV2::Replaced => SemanticWriteOutcomeV2::Replaced,
        },
        projection_state: match result.projection {
            RecordProjectionStatusV2::Current(_) => ProjectionStateV2::Current,
            RecordProjectionStatusV2::RepairRequired(_) => ProjectionStateV2::RepairRequired,
            RecordProjectionStatusV2::Stale { .. } => ProjectionStateV2::Stale,
        },
    }
}

/// Lets the owner pick which pending item to review.
///
/// This used to take the first item of the queue and nothing else. The queue is
/// keyed by Asset id — a content hash — so "first" meant the lexicographically
/// smallest hash, the same item every single time, and `[d] 나중에` leaves an
/// item exactly where it was. With eight items pending, seven of them could not
/// be reached from the home screen at all; the only route was `mko review <ID>`
/// with a 70-character id obtained from `mko queue`, which is a hidden command.
/// How many times a stray keystroke may be corrected before the owner is
/// sent back to the shell. Bounded so garbage on a non-interactive stdin
/// cannot spin this forever.
const REVIEW_SELECTION_ATTEMPTS: usize = 3;

fn choose_review_item(repository: &Path) -> Result<Option<String>, MkoError> {
    let queue = derive_queue_v2(repository)?;
    let items = queue.items;
    let (first, rest) = items
        .split_first()
        .ok_or_else(|| MkoError::new("review_queue_empty", "there is no pending review item"))?;
    if rest.is_empty() {
        return Ok(Some(first.item_id.clone()));
    }

    println!();
    println!("검토 대기 {}개", items.len());
    for (index, item) in items.iter().enumerate() {
        println!(
            "{}. {} [{} / {}]",
            index + 1,
            item.title,
            item_type_label(&item.item_type),
            state_label(&item.state),
        );
        println!("   다음 작업: {}", action_label(&item.next_action));
    }
    if !queue.scan_complete || queue.remaining != 0 {
        println!(
            "주의: 검색이 완전하지 않습니다 (남은 항목: {}).",
            queue.remaining
        );
    }
    println!();

    for attempt in 1..=REVIEW_SELECTION_ATTEMPTS {
        print!("검토할 항목 번호 [Enter: 1번 · q: 닫기] › ");
        std::io::stdout()
            .flush()
            .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
        let mut selected = String::new();
        let read = std::io::stdin()
            .read_line(&mut selected)
            .map_err(|error| MkoError::new("review_input_failed", error.to_string()))?;
        // Zero bytes is end of input, not an empty answer. Treating it as
        // Enter reopened the very defect this screen replaced: a caller with
        // a closed stdin silently got item 1 — the lexicographically smallest
        // hash — every time. A closed input opens nothing.
        if read == 0 {
            return Err(MkoError::new(
                "review_input_closed",
                "선택 입력이 닫혔습니다; 대화형 터미널에서 `mko`를 다시 여세요",
            ));
        }
        let selected = selected.trim();
        if selected.is_empty() {
            return Ok(Some(first.item_id.clone()));
        }
        if matches!(selected, "q" | "Q") {
            return Ok(None);
        }
        if let Some(item) = selected
            .parse::<usize>()
            .ok()
            .filter(|index| (1..=items.len()).contains(index))
            .map(|index| &items[index - 1])
        {
            return Ok(Some(item.item_id.clone()));
        }
        if attempt < REVIEW_SELECTION_ATTEMPTS {
            println!(
                "표시된 항목 번호를 입력하세요 (1-{}, Enter: 1번, q: 닫기)",
                items.len()
            );
        }
    }
    Err(MkoError::new(
        "review_selection_invalid",
        "표시된 항목 번호를 입력하세요",
    ))
}

/// What is left after a decision. Finishing one item and being told nothing
/// about the rest is the point at which the owner has to go find out for
/// themselves whether they are done.
fn report_review_remaining(repository: &Path) {
    let Ok(queue) = derive_queue_v2(repository) else {
        return;
    };
    match queue.items.len() {
        0 => println!("검토할 항목이 모두 끝났습니다."),
        remaining => {
            println!("검토 대기 {remaining}개가 남았습니다: `mko`로 이어서 볼 수 있습니다.")
        }
    }
}

pub fn review(
    repository: &Path,
    stable_id: Option<&str>,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let selected_id = match stable_id {
        Some(stable_id) => stable_id.to_owned(),
        None => match choose_review_item(repository)? {
            Some(selected_id) => selected_id,
            None => {
                println!("아무것도 열지 않았습니다.");
                return Ok(());
            }
        },
    };
    match publish_tty_review_v2(repository, &selected_id, clock)? {
        TtyReviewOutcomeV2::Approved(publication) => {
            report_review_publication(&publication)?;
            report_review_remaining(repository);
        }
        TtyReviewOutcomeV2::ChangesRequested(publication) => {
            println!("수정을 요청했습니다: {}", publication.record.id);
            println!(
                "이 항목은 수정 요청 상태로 대기열에 남아 있고, 다음 초안이 준비되면 다시 검토할 수 있습니다."
            );
            report_review_remaining(repository);
        }
        TtyReviewOutcomeV2::Deferred(publication) => {
            println!("나중에 보기로 했습니다: {}", publication.record.id);
            println!("이 항목은 검토 대기열에 그대로 남아 있습니다.");
            report_review_remaining(repository);
        }
        TtyReviewOutcomeV2::Cancelled => {
            println!("아무것도 바꾸지 않았습니다.");
            println!("이 항목은 검토 대기열에 남아 있습니다: `mko`로 다시 열 수 있습니다.");
        }
    }
    Ok(())
}

fn report_review_publication(
    publication: &mko_core::review_v2::ReviewPublicationV2,
) -> Result<(), MkoError> {
    let mut blocked = Vec::new();
    for projection in &publication.projections {
        match projection {
            RecordProjectionStatusV2::Current(_) => {}
            RecordProjectionStatusV2::RepairRequired(result) => {
                let recovery = result.recovery.as_ref();
                blocked.push(format!(
                    "{}: user edit preserved; inspect {}, resolve or relocate the edit, then run `mko dashboard` to verify readiness",
                    result.path.display(),
                    recovery
                        .map(|recovery| recovery.diff_path.display().to_string())
                        .unwrap_or_else(|| "the generated recovery diff".into())
                ));
            }
            RecordProjectionStatusV2::Stale { path, error } => blocked.push(format!(
                "{}: stale ({}) — run `mko dashboard --repair`, then `mko check`",
                path.display(),
                error.code()
            )),
        }
    }
    if blocked.is_empty() {
        println!("approved {} (readiness current)", publication.record.id);
    } else {
        println!(
            "review event published {}, but readiness is blocked by {} projection(s)",
            publication.record.id,
            blocked.len()
        );
        let stderr = io::stderr();
        let mut output = stderr.lock();
        for blocker in blocked {
            writeln!(output, "- {blocker}").map_err(|error| output_error(error.to_string()))?;
        }
        output
            .flush()
            .map_err(|error| output_error(error.to_string()))?;
    }
    Ok(())
}

fn item_type_label(value: &QueueItemTypeV2) -> &'static str {
    match value {
        QueueItemTypeV2::Source => "Source",
        QueueItemTypeV2::Knowledge => "Knowledge",
        QueueItemTypeV2::Combined => "Source + Knowledge",
    }
}

fn state_label(value: &QueueItemStateV2) -> &'static str {
    match value {
        QueueItemStateV2::Unreviewed => "미검토",
        QueueItemStateV2::Deferred => "보류",
        QueueItemStateV2::ChangesRequested => "수정 요청",
        QueueItemStateV2::RevisedUnreviewed => "수정 후 미검토",
        QueueItemStateV2::Blocked => "차단됨",
    }
}

fn action_label(value: &QueueNextActionV2) -> &'static str {
    match value {
        QueueNextActionV2::Display => "내용 확인",
        QueueNextActionV2::Regenerate => "수정본 생성",
        QueueNextActionV2::Diagnose => "문제 진단",
    }
}

fn json_target_state(value: ReviewCardTargetStateV2) -> ReviewTargetStateV2 {
    match value {
        ReviewCardTargetStateV2::Unreviewed => ReviewTargetStateV2::Unreviewed,
        ReviewCardTargetStateV2::Deferred => ReviewTargetStateV2::Deferred,
        ReviewCardTargetStateV2::ChangesRequested => ReviewTargetStateV2::ChangesRequested,
        ReviewCardTargetStateV2::RevisedUnreviewed => ReviewTargetStateV2::RevisedUnreviewed,
        ReviewCardTargetStateV2::Approved => ReviewTargetStateV2::Approved,
        ReviewCardTargetStateV2::Blocked => ReviewTargetStateV2::Blocked,
    }
}

fn read_decision_input(path: &Path) -> Result<ReviewSessionDecisionInputV2, MkoError> {
    read_json_input(
        path,
        "review_feedback_input_invalid",
        MAX_REVIEW_INPUT_BYTES,
    )
}

fn read_json_input<T: DeserializeOwned>(
    path: &Path,
    error_code: &str,
    maximum_bytes: u64,
) -> Result<T, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    let file = options.open(path).map_err(|error| {
        MkoError::new(
            error_code,
            format!("cannot open the decision input without following links: {error}"),
        )
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new(error_code, error.to_string()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum_bytes
    {
        return Err(MkoError::new(
            error_code,
            "the decision input must be a bounded regular non-symlink file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new(error_code, error.to_string()))?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(MkoError::new(
            error_code,
            "the decision input exceeds its bounded size",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| MkoError::new(error_code, error.to_string()))
}

#[cfg(target_os = "linux")]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x20_000 | 0x800);
}

#[cfg(target_os = "macos")]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x100 | 0x4);
}

#[cfg(windows)]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x0020_0000);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_nofollow(_options: &mut OpenOptions) {}

fn output_error(message: String) -> MkoError {
    MkoError::new("human_output_failed", message)
}
