use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use mko_core::{
    add::{AddInput, AddRequest, AddRunResult, BackupAttestation, add as add_input},
    approve::{ApproveSourceRequest, approve_source},
    asset_v2::{
        AssetRegistrationOutcomeV2, HydrationConfirmationV2, RegisterAssetRequestV2,
        RegisterInboxAssetsRequestV2, register_inbox_pdf_assets_v2, register_pdf_asset_v2,
    },
    check::{CheckReport, CheckRequest, check_repository},
    clock::{Clock, SystemClock},
    config_v2::PerspectiveV2,
    context::{
        ResolveContextRequest, ResolvedPersonalContext, SystemPlatformEnvironment,
        resolve_personal_context,
    },
    dashboard_v2::{
        DashboardCanonicalStateV2, DashboardFileKindV2, DashboardFileStateV2, DashboardOutcomeV2,
        DashboardProjectionStateV2, inspect_dashboard_v2, repair_dashboard_v2,
    },
    doctor::{DoctorRequest, SystemDoctorEnvironment, diagnose},
    error::MkoError,
    home::{
        HomeNextAction, HomeReport, RepositoryGeneration, detect_repository_generation,
        inspect_home,
    },
    hooks::install_hooks,
    inbox::{InboxScanRequest, InboxScanResult, scan_inbox},
    json_v1::{
        AddData, AddPayload, CheckData, ConceptMatchData, DiagnosticData, DoctorCheckData,
        DoctorCheckStatus, DoctorData, DraftOutcome, JsonV1Command, JsonV1Success,
        KnowledgeConceptSummary, KnowledgeListData, KnowledgePendingItemData, KnowledgeReviewData,
        KnowledgeReviewDecision, KnowledgeReviewItemData, KnowledgeReviewStatusData,
        KnowledgeSearchData, KnowledgeShowData, KnowledgeWriteData, KnowledgeWriteOutcome,
        NextAction, PrepareData, Recovery, RecoveryKind, SuccessResult, UserState, WriteDraftData,
    },
    json_v2::{
        AddBatchDataV2, AddBatchItemErrorV2, AddBatchItemV2, AddBatchWarningV2, AddDataV2,
        AddOutcomeV2, AddSingleDataV2, DashboardCanonicalStateDataV2, DashboardDataV2,
        DashboardFileDataV2, DashboardFileKindDataV2, DashboardFileStateDataV2,
        DashboardProjectionStateDataV2, DoctorCheckDataV2, DoctorCheckStatusV2, DoctorDataV2,
        HandshakeDataV2, JsonV2Command, JsonV2Success, NextActionV2, SetupApplyDataV2,
    },
    knowledge::{
        ConceptKind, ConceptMatch, KnowledgeSearchQuery, WriteKnowledgeRequest, approve_knowledge,
        list_knowledge, list_unreviewed_knowledge, search_knowledge, write_knowledge_note,
    },
    model::AssetStatus,
    pdf::{ExtractionWorkerResponse, extract_pdf_pages_from_reader, worker_executable},
    perspective_v2::{prepare_perspective_confirmation_v2, publish_perspective_confirmation_v2},
    prepare::{PrepareRequest, prepare_source},
    provider_scan::MonotonicElapsedClock,
    queue_v2::{
        KnowledgeSearchLayerV2, ResurfacedKnowledgeStateV2, resurface_knowledge_by_perspective_v2,
        search_approved_knowledge_by_perspective_v2, summarize_home_queue_v2,
    },
    quick_note_v2::{
        QuickNotePublicationOutcomeV2, prepare_quick_note_v2, publish_quick_note_v2,
        search_quick_notes_v2,
    },
    records_v2::RecordWriteOutcomeV2,
    registry::{
        AssetOperationRequest, CaptureRequest, accept_changed_asset, capture_asset, inspect_asset,
        repair_lineage,
    },
    resurface_history_v2::record_resurfaced_knowledge_open_v2,
    review::{ReviewOutcome, review as review_pending},
    setup::{
        SetupRequest, SystemSetupWriter, apply_setup, detect_google_drive_roots, preflight_setup,
    },
    setup_plan_v2::{apply_setup_plan_v2_tty, create_setup_plan_v2},
    setup_v2::{SetupPersonalV2Request, setup_personal_v2},
    source::{
        RepairSourceStateRequest, WriteSourceRequest, repair_source_state, write_source_draft,
    },
    status::{StatusReport, status_from_inbox},
};

use crate::{
    batch_add_data,
    output::{
        emit_encoded_json, emit_json_v1, emit_json_v1_failure, emit_json_v2, emit_json_v2_failure,
        emit_json_value, emit_legacy_json_error, json_v2_next_action,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Human,
    JsonV1,
    JsonV2,
}

#[derive(Parser)]
#[command(
    name = "mko",
    version = mko_core::version::PRODUCT_VERSION,
    about = "자료를 지식으로 정리하고 다시 찾는 개인 지식 홈"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

// This parser is deliberately frozen at the v0.1 argument contract. It is used
// only to render legacy `--json` parse errors, whose serialized bytes are an API.
#[derive(Parser)]
#[command(name = "mko", version = mko_core::version::PRODUCT_VERSION, about = "My Knowledge OS deterministic core")]
struct LegacyCli {
    #[command(subcommand)]
    command: LegacyCommand,
}

#[derive(Subcommand)]
enum LegacyCommand {
    Asset {
        #[command(subcommand)]
        command: LegacyAssetCommand,
    },
    Source {
        #[command(subcommand)]
        command: LegacySourceCommand,
    },
    Check(LegacyCheckArgs),
    Human {
        #[command(subcommand)]
        command: LegacyHumanCommand,
    },
    Hooks {
        #[command(subcommand)]
        command: LegacyHooksCommand,
    },
    #[command(name = "__extract-pdf", hide = true)]
    ExtractPdf,
}

#[derive(Subcommand)]
enum LegacySourceCommand {
    Prepare(LegacyPrepareArgs),
    WriteDraft(LegacyWriteDraftArgs),
    RepairState(LegacyRepairStateArgs),
}

#[derive(Subcommand)]
enum LegacyAssetCommand {
    Capture(LegacyCaptureArgs),
    Inspect(LegacyAssetOperationArgs),
    AcceptChange(LegacyAssetOperationArgs),
    RepairLineage(LegacyAssetOperationArgs),
}

#[derive(Subcommand)]
enum LegacyHumanCommand {
    ApproveSource(LegacyApproveSourceArgs),
}

#[derive(Subcommand)]
enum LegacyHooksCommand {
    Install(LegacyHookInstallArgs),
}

#[derive(Args)]
struct LegacyCaptureArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    local_config: Option<PathBuf>,
    #[arg(long)]
    file: PathBuf,
    #[arg(long)]
    title: Option<String>,
    #[arg(long = "domain")]
    domains: Vec<String>,
    #[arg(long)]
    slug: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct LegacyAssetOperationArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    local_config: Option<PathBuf>,
    #[arg(long)]
    asset_id: String,
    #[arg(long)]
    clear_stale_lock: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct LegacyCheckArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    staged: bool,
}

#[derive(Args)]
struct LegacyApproveSourceArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    source_id: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct LegacyHookInstallArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct LegacyPrepareArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    local_config: Option<PathBuf>,
    #[arg(long)]
    asset_id: String,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    clear_stale_lock: bool,
}

#[derive(Args)]
struct LegacyWriteDraftArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    response: PathBuf,
    #[arg(long)]
    slug: Option<String>,
    #[arg(long)]
    replace_pending: bool,
    #[arg(long)]
    clear_stale_lock: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct LegacyRepairStateArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    asset_id: String,
    #[arg(long)]
    clear_stale_lock: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    Handshake(HandshakeArgs),
    #[command(hide = true)]
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
    /// 처음 사용할 저장소와 자료 폴더를 연결합니다
    Setup(SetupArgs),
    /// Inbox의 새 자료를 등록합니다
    Add(AddArgs),
    /// 승인된 지식에서 내용을 찾습니다
    Find(FindArgs),
    /// 내 문장을 그대로 빠르게 저장합니다
    Remember(RememberArgs),
    #[command(hide = true)]
    Perspective(PerspectiveArgs),
    #[command(hide = true)]
    Asset {
        #[command(subcommand)]
        command: AssetCommand,
    },
    #[command(hide = true)]
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    #[command(hide = true)]
    Check(CheckArgs),
    /// 연결과 저장소 상태를 점검합니다
    Doctor(DoctorArgs),
    #[command(hide = true)]
    Inbox(InboxArgs),
    #[command(hide = true)]
    Status(StatusArgs),
    #[command(hide = true)]
    Queue(QueueArgs),
    #[command(hide = true)]
    Show(ShowArgs),
    #[command(name = "review-open", hide = true)]
    ReviewOpen(ReviewOpenArgs),
    #[command(name = "review-feedback", hide = true)]
    ReviewFeedback(ReviewFeedbackArgs),
    /// 대기 중인 초안을 읽고 승인하거나 돌려보냅니다
    Review(ReviewArgs),
    #[command(hide = true)]
    Dashboard(DashboardArgs),
    #[command(hide = true)]
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommand,
    },
    #[command(hide = true)]
    Human {
        #[command(subcommand)]
        command: HumanCommand,
    },
    #[command(hide = true)]
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    #[command(name = "__extract-pdf", hide = true)]
    ExtractPdf,
}

#[derive(Args)]
struct HandshakeArgs {
    #[arg(long = "skill-version")]
    skill_version: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}

#[derive(Subcommand)]
enum SchemaCommand {
    List(SchemaListArgs),
    Show(SchemaShowArgs),
}

#[derive(Args)]
struct SchemaListArgs {
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}

#[derive(Args)]
struct SchemaShowArgs {
    name: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}

#[derive(Args)]
struct FindArgs {
    term: String,
    #[arg(long, value_enum)]
    perspective: Option<PerspectiveArg>,
    #[arg(long)]
    repo: Option<PathBuf>,
}

#[derive(Args)]
struct RememberArgs {
    text: Option<String>,
    #[arg(long)]
    repo: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PerspectiveArg {
    Life,
    Learning,
    Technical,
    Project,
    Investment,
}

impl From<PerspectiveArg> for PerspectiveV2 {
    fn from(value: PerspectiveArg) -> Self {
        match value {
            PerspectiveArg::Life => Self::Life,
            PerspectiveArg::Learning => Self::Learning,
            PerspectiveArg::Technical => Self::Technical,
            PerspectiveArg::Project => Self::Project,
            PerspectiveArg::Investment => Self::Investment,
        }
    }
}

#[derive(Args)]
struct PerspectiveArgs {
    knowledge_id: String,
    #[arg(long = "set", required = true, value_enum)]
    perspectives: Vec<PerspectiveArg>,
    #[arg(long)]
    repo: Option<PathBuf>,
}

#[derive(Subcommand)]
enum SourceCommand {
    Prepare(PrepareArgs),
    WriteDraft(WriteDraftArgs),
    RepairState(RepairStateArgs),
}
#[derive(Subcommand)]
enum KnowledgeCommand {
    Write(KnowledgeWriteArgs),
    Review(KnowledgeReviewArgs),
    Search(KnowledgeSearchArgs),
    Show(KnowledgeShowArgs),
    List(KnowledgeListArgs),
}
#[derive(Subcommand)]
enum AssetCommand {
    Capture(CaptureArgs),
    Inspect(AssetOperationArgs),
    AcceptChange(AssetOperationArgs),
    RepairLineage(AssetOperationArgs),
}
#[derive(Subcommand)]
enum HumanCommand {
    ApproveSource(ApproveSourceArgs),
}
#[derive(Subcommand)]
enum HooksCommand {
    Install(HookInstallArgs),
}

#[derive(Args)]
struct SetupArgs {
    #[command(subcommand)]
    command: Option<SetupCommand>,
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long)]
    drive_root: Option<PathBuf>,
    #[arg(long)]
    replace_profile: bool,
}

#[derive(Subcommand)]
enum SetupCommand {
    Plan(SetupPlanArgs),
    Apply(SetupApplyArgs),
}

#[derive(Args)]
struct SetupPlanArgs {
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long)]
    drive_root: Option<PathBuf>,
    #[arg(long)]
    replace_profile: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::JsonV2)]
    format: OutputFormat,
}

#[derive(Args)]
struct SetupApplyArgs {
    #[arg(long)]
    plan: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::JsonV2)]
    format: OutputFormat,
}
#[derive(Args)]
struct AddArgs {
    #[arg(required_unless_present = "inbox", conflicts_with = "inbox")]
    file: Option<PathBuf>,
    #[arg(long)]
    inbox: bool,
    #[arg(long)]
    verified_backup: bool,
    #[arg(long)]
    temporary_source: bool,
    #[arg(long)]
    confirm_download: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}
#[derive(Args)]
struct DashboardArgs {
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long)]
    repair: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}
#[derive(Args)]
struct CaptureArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    local_config: Option<PathBuf>,
    #[arg(long)]
    file: PathBuf,
    #[arg(long)]
    title: Option<String>,
    #[arg(long = "domain")]
    domains: Vec<String>,
    #[arg(long)]
    slug: Option<String>,
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
struct AssetOperationArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    local_config: Option<PathBuf>,
    #[arg(long)]
    asset_id: String,
    #[arg(long)]
    clear_stale_lock: bool,
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
struct CheckArgs {
    #[arg(
        long,
        required_unless_present = "format",
        required_if_eq("format", "human")
    )]
    repo: Option<PathBuf>,
    #[arg(long, conflicts_with = "format")]
    json: bool,
    #[arg(long)]
    staged: bool,
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}
#[derive(Args)]
struct DoctorArgs {
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Take over a lock left behind by an interrupted operation. Core refuses
    /// while the owning process is still running.
    #[arg(long)]
    clear_stale_lock: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}
#[derive(Args)]
struct InboxArgs {
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}
#[derive(Args)]
struct StatusArgs {
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}
#[derive(Args)]
struct QueueArgs {
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}
#[derive(Args)]
struct ShowArgs {
    stable_id: String,
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}
#[derive(Args)]
struct ReviewOpenArgs {
    stable_id: String,
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::JsonV2)]
    format: OutputFormat,
}
#[derive(Args)]
struct ReviewFeedbackArgs {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::JsonV2)]
    format: OutputFormat,
}
#[derive(Args)]
struct ReviewArgs {
    #[arg(
        value_name = "TARGET_OR_QUEUE_ID",
        help = "Source/Knowledge ID approves only that record; a queue item ID (or no ID) approves all actionable records in the displayed card"
    )]
    stable_id: Option<String>,
    #[arg(long)]
    repo: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ConceptKindArg {
    Definition,
    Formula,
    Concept,
    Result,
    Theorem,
}
impl From<ConceptKindArg> for ConceptKind {
    fn from(value: ConceptKindArg) -> Self {
        match value {
            ConceptKindArg::Definition => ConceptKind::Definition,
            ConceptKindArg::Formula => ConceptKind::Formula,
            ConceptKindArg::Concept => ConceptKind::Concept,
            ConceptKindArg::Result => ConceptKind::Result,
            ConceptKindArg::Theorem => ConceptKind::Theorem,
        }
    }
}
#[derive(Args)]
struct KnowledgeWriteArgs {
    #[arg(
        long,
        required_unless_present = "format",
        required_if_eq("format", "human")
    )]
    repo: Option<PathBuf>,
    #[arg(long)]
    asset_id: String,
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    response: PathBuf,
    #[arg(long)]
    replace: bool,
    #[arg(long)]
    expected_revision: Option<String>,
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}
#[derive(Args)]
struct KnowledgeReviewArgs {
    #[arg(
        long,
        required_unless_present = "format",
        required_if_eq("format", "human")
    )]
    repo: Option<PathBuf>,
    #[arg(long)]
    asset_id: Option<String>,
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}
#[derive(Args)]
struct KnowledgeSearchArgs {
    #[arg(
        long,
        required_unless_present = "format",
        required_if_eq("format", "human")
    )]
    repo: Option<PathBuf>,
    term: String,
    #[arg(long, value_enum)]
    kind: Option<ConceptKindArg>,
    #[arg(long)]
    tag: Option<String>,
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}
#[derive(Args)]
struct KnowledgeShowArgs {
    #[arg(
        long,
        required_unless_present = "format",
        required_if_eq("format", "human")
    )]
    repo: Option<PathBuf>,
    #[arg(long)]
    asset_id: String,
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}
#[derive(Args)]
struct KnowledgeListArgs {
    #[arg(
        long,
        required_unless_present = "format",
        required_if_eq("format", "human")
    )]
    repo: Option<PathBuf>,
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}
#[derive(Args)]
struct ApproveSourceArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    source_id: String,
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
struct HookInstallArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
struct PrepareArgs {
    #[arg(
        long,
        required_unless_present = "format",
        required_if_eq("format", "human")
    )]
    repo: Option<PathBuf>,
    #[arg(long)]
    local_config: Option<PathBuf>,
    #[arg(long)]
    asset_id: String,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    confirm_download: bool,
    #[arg(long)]
    clear_stale_lock: bool,
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}
#[derive(Args)]
struct WriteDraftArgs {
    #[arg(
        long,
        required_unless_present = "format",
        required_if_eq("format", "human")
    )]
    repo: Option<PathBuf>,
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    response: PathBuf,
    #[arg(long)]
    slug: Option<String>,
    #[arg(long)]
    replace_pending: bool,
    #[arg(long)]
    expected_revision: Option<String>,
    #[arg(long)]
    clear_stale_lock: bool,
    #[arg(long, conflicts_with = "format")]
    json: bool,
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}
#[derive(Args)]
struct RepairStateArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    asset_id: String,
    #[arg(long)]
    clear_stale_lock: bool,
    #[arg(long)]
    json: bool,
}

pub fn entry() {
    let args = std::env::args_os().collect::<Vec<_>>();
    match Cli::try_parse_from(&args) {
        Ok(cli) => {
            let json_v1_command = json_v1_command(&cli);
            let json_v2_command = json_v2_command(&cli);
            let legacy_check_requested = matches!(
                &cli.command,
                Some(Command::Check(CheckArgs {
                    format: None | Some(OutputFormat::Human),
                    ..
                }))
            );
            match run(cli) {
                Ok(Exit::Success) => {}
                Ok(Exit::ValidationFailed) => std::process::exit(1),
                Err(error) => {
                    if let Some(command) = json_v2_command {
                        emit_json_v2_failure_or_stderr(command, &error);
                    } else if let Some(command) = json_v1_command {
                        emit_json_v1_failure_or_stderr(command, &error);
                    } else {
                        emit_legacy_error(
                            error.code(),
                            error.message(),
                            args.iter().any(|arg| arg == "--json"),
                        );
                    }
                    std::process::exit(if legacy_check_requested { 2 } else { 1 });
                }
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
        }
        Err(error) => {
            let usage = legacy_usage_error(&args)
                .unwrap_or_else(|| MkoError::new("usage", error.to_string()));
            if let Some(command) = json_v2_command_from_invalid_arguments(&args) {
                emit_json_v2_failure_or_stderr(command, &usage);
            } else if let Some(command) = json_v1_command_from_invalid_arguments(&args) {
                emit_json_v1_failure_or_stderr(command, &usage);
            } else {
                emit_legacy_error(
                    usage.code(),
                    usage.message(),
                    legacy_json_requested_from_invalid_arguments(&args),
                );
            }
            std::process::exit(2);
        }
    }
}

enum Exit {
    Success,
    ValidationFailed,
}

fn home() -> Result<(), MkoError> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(MkoError::new(
            "home_tty_required",
            "인자를 생략한 mko 홈은 대화형 터미널에서만 열립니다; 자동화에서는 명령을 명시하세요",
        ));
    }
    let context = match resolve_context(None) {
        Ok(context) => context,
        Err(error) if error.code() == "context_not_found" => {
            println!("아직 연결된 My Knowledge OS가 없습니다.");
            println!("Codex에서 “MKO 시작해줘”라고 말하거나 `mko setup`을 실행하세요.");
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let report = inspect_home(
        &context.repository_root,
        &context.provider_root,
        &MonotonicElapsedClock::start(),
    )?;
    render_home(&report);
    print!("선택 › ");
    std::io::stdout()
        .flush()
        .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
    let mut selection = String::new();
    std::io::stdin()
        .read_line(&mut selection)
        .map_err(|error| MkoError::new("home_input_failed", error.to_string()))?;
    match (&report, selection.trim()) {
        (_, "" | "q" | "Q") => Ok(()),
        (HomeReport::Legacy(report), "1") => legacy_home_action(report, &context.repository_root),
        (HomeReport::Legacy(_), "2") => {
            println!("새 v3 저장소는 `mko setup plan`으로 계획을 먼저 확인할 수 있습니다.");
            println!("기존 자료는 자동으로 옮기거나 바꾸지 않습니다.");
            Ok(())
        }
        (HomeReport::V3(_), "1") => add(AddArgs {
            file: None,
            inbox: true,
            verified_backup: false,
            temporary_source: false,
            confirm_download: false,
            format: OutputFormat::Human,
        }),
        (HomeReport::V3(_), "2") => review(ReviewArgs {
            stable_id: None,
            repo: Some(context.repository_root.clone()),
        }),
        (HomeReport::V3(_), "3") => {
            print!("찾을 내용 › ");
            std::io::stdout()
                .flush()
                .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
            let mut term = String::new();
            std::io::stdin()
                .read_line(&mut term)
                .map_err(|error| MkoError::new("home_input_failed", error.to_string()))?;
            find(FindArgs {
                term: term.trim().to_owned(),
                perspective: None,
                repo: Some(context.repository_root),
            })
        }
        (HomeReport::V3(_), "4") => remember(RememberArgs {
            text: None,
            repo: Some(context.repository_root),
        }),
        (HomeReport::V3(report), "5") if report.blocked > 0 => doctor(DoctorArgs {
            repo: Some(context.repository_root),
            clear_stale_lock: false,
            format: OutputFormat::Human,
        }),
        (HomeReport::V3(_), "5") => resurface(&context.repository_root),
        _ => Err(MkoError::new(
            "home_selection_invalid",
            "표시된 번호나 q를 입력하세요",
        )),
    }
}

fn render_home(report: &HomeReport) {
    println!("My Knowledge OS");
    match report {
        HomeReport::Legacy(report) => {
            println!("기존 지식베이스를 읽기 전용으로 열었습니다.");
            println!(
                "새 자료 {} · 등록 {} · 검토 {} · 완료 {} · 문제 {}",
                report.new_material,
                report.registered,
                report.review_pending,
                report.complete,
                report.blocked.saturating_add(report.incomplete)
            );
            println!("추천: {}", legacy_action_label(report));
            println!();
            println!("[1] {}", legacy_action_label(report));
            println!("[2] 새 v3 설정 계획 안내");
            println!("[q] 닫기");
        }
        HomeReport::V3(report) => {
            let next_action = HomeReport::V3(report.clone()).next_action();
            println!(
                "새 자료 {} · 정리 중 {} · 검토 {} · 수정 필요 {} · 승인된 지식 {} · 문제 {}",
                report.new_material,
                report.in_progress,
                report.review_pending,
                report.changes_requested,
                report.approved_knowledge,
                report.blocked
            );
            println!(
                "추천: {}",
                match next_action {
                    HomeNextAction::Add if report.new_material == 0 => "멈춘 자료 계속 정리",
                    HomeNextAction::Add => "새 자료 정리",
                    HomeNextAction::Review => "검토 계속",
                    HomeNextAction::Repair => "문제 확인",
                    HomeNextAction::None => "필요한 지식 찾기",
                }
            );
            println!();
            if report.in_progress > 0 {
                println!(
                    "[1] 자료 정리 (새 자료 {} · 정리하다 멈춘 자료 {})",
                    report.new_material, report.in_progress
                );
            } else {
                println!("[1] 새 자료 정리");
            }
            println!("[2] 검토 계속");
            println!("[3] 지식 찾기");
            println!("[4] 빠른 메모");
            if report.blocked > 0 {
                println!("[5] 문제 확인");
            } else {
                println!("[5] 다시 볼 지식");
            }
            println!("[q] 닫기");
        }
    }
}

fn legacy_action_label(report: &mko_core::home::LegacyHomeReport) -> &'static str {
    if report.blocked > 0 {
        "문제 확인"
    } else if report.review_pending > 0 {
        "검토 계속"
    } else if report.incomplete > 0 || report.registered > 0 {
        "등록된 자료 계속 정리"
    } else if report.new_material > 0 {
        "새 자료 정리"
    } else {
        "현재 형식 안내"
    }
}

fn legacy_home_action(
    report: &mko_core::home::LegacyHomeReport,
    repository: &Path,
) -> Result<(), MkoError> {
    if report.blocked > 0 {
        return doctor(DoctorArgs {
            repo: Some(repository.to_path_buf()),
            clear_stale_lock: false,
            format: OutputFormat::Human,
        });
    }
    if report.review_pending > 0 {
        return review(ReviewArgs {
            stable_id: None,
            repo: Some(repository.to_path_buf()),
        });
    }
    if report.incomplete > 0 || report.registered > 0 {
        println!("Codex에서 “등록된 자료 계속 정리해줘”라고 요청하세요.");
        println!("기존 저장소는 자동 변환하지 않고 현재 자료의 다음 초안만 만듭니다.");
        return Ok(());
    }
    if report.new_material > 0 {
        return add(AddArgs {
            file: None,
            inbox: true,
            verified_backup: false,
            temporary_source: false,
            confirm_download: false,
            format: OutputFormat::Human,
        });
    }
    println!("현재 처리할 항목이 없습니다. `mko find \"찾을 내용\"`으로 지식을 찾을 수 있습니다.");
    Ok(())
}

/// Finding nothing is a normal outcome, but ending there hides the reason.
/// Approved knowledge is the only thing search covers, so when the shelf is
/// empty or everything is still waiting on the owner, say which it is.
fn report_search_dead_end(repository: &Path) {
    let Ok(summary) = summarize_home_queue_v2(repository) else {
        return;
    };
    if summary.approved_knowledge == 0 {
        println!("아직 승인된 지식이 없습니다. 검색은 승인된 지식만 찾습니다.");
    }
    let waiting = summary.review_pending + summary.changes_requested;
    if waiting > 0 {
        println!("검토를 기다리는 항목이 {waiting}개 있습니다.");
        println!("`mko`를 열어 검토를 계속하면 검색에도 나타납니다.");
    } else if summary.approved_knowledge == 0 {
        println!("`mko`를 열어 새 자료를 정리하는 것부터 시작할 수 있습니다.");
    }
}

fn find(arguments: FindArgs) -> Result<(), MkoError> {
    let repository = setup_repository(arguments.repo)?;
    let perspective = arguments.perspective.map(Into::into);
    match detect_repository_generation(&repository)? {
        RepositoryGeneration::LegacyV1 => {
            if perspective.is_some() {
                return Err(MkoError::new(
                    "perspective_v3_required",
                    "관점 필터는 v3 Personal KB에서 사용할 수 있습니다",
                ));
            }
            let matches = search_knowledge(
                &repository,
                &KnowledgeSearchQuery {
                    term: arguments.term,
                    kind: None,
                    tag: None,
                },
            )?;
            if matches.is_empty() {
                println!("승인된 지식에서 찾지 못했습니다.");
            } else {
                for concept in matches {
                    println!("{} · {}", concept.title, concept.name);
                }
            }
        }
        RepositoryGeneration::V3 => {
            let matches = search_approved_knowledge_by_perspective_v2(
                &repository,
                &arguments.term,
                perspective,
            )?;
            let notes = if perspective.is_none() {
                search_quick_notes_v2(&repository, &arguments.term)?
            } else {
                Vec::new()
            };
            if matches.is_empty() && notes.is_empty() {
                println!("승인된 지식에서 찾지 못했습니다.");
                report_search_dead_end(&repository);
            } else {
                for item in matches {
                    println!(
                        "[{}] {}",
                        match item.layer {
                            KnowledgeSearchLayerV2::GroundedEvidence => "문서 근거",
                            KnowledgeSearchLayerV2::LlmAnalysis => "LLM 분석",
                            KnowledgeSearchLayerV2::CounterargumentOrUncertainty => {
                                "반론·불확실성"
                            }
                        },
                        item.title
                    );
                    println!("  {}", compact_excerpt(&item.body, 140));
                    if !item.perspectives.is_empty() {
                        println!(
                            "  관점: {}",
                            item.perspectives
                                .iter()
                                .map(PerspectiveV2::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    if !item.locators.is_empty() {
                        println!("  근거: {}", item.locators.join(", "));
                    }
                    // A 140-character excerpt is a pointer, not an answer. The
                    // projection is the readable document, so name it.
                    println!(
                        "  전체 보기: {}",
                        mko_core::projection_v2::record_projection_relative_path_v2(
                            mko_core::projection_v2::ProjectionRecordTypeV2::Knowledge,
                            &item.knowledge_id,
                        )
                    );
                }
                for note in notes {
                    println!("[내 생각] {}", compact_excerpt(&note.text, 140));
                }
            }
        }
    }
    Ok(())
}

fn remember(arguments: RememberArgs) -> Result<(), MkoError> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(MkoError::new(
            "remember_tty_required",
            "빠른 메모는 정확한 원문 확인을 위해 실제 터미널에서만 저장할 수 있습니다",
        ));
    }
    let repository = setup_repository(arguments.repo)?;
    if detect_repository_generation(&repository)? != RepositoryGeneration::V3 {
        return Err(MkoError::new(
            "remember_v3_required",
            "빠른 메모는 v3 Personal KB에서 사용할 수 있습니다",
        ));
    }
    let text = match arguments.text {
        Some(text) => text,
        None => {
            print!("무엇을 기억할까요?\n› ");
            std::io::stdout()
                .flush()
                .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
            let mut text = String::new();
            std::io::stdin()
                .read_line(&mut text)
                .map_err(|error| MkoError::new("remember_input_failed", error.to_string()))?;
            text.trim_end_matches(['\r', '\n']).to_owned()
        }
    };
    let prepared = prepare_quick_note_v2(&text, SystemClock.now_utc())?;
    if prepared.input_changed {
        println!("줄바꿈 또는 유니코드를 다음 저장 형태로 정규화했습니다.");
    }
    println!();
    println!("{}", prepared.note.text);
    println!();
    print!("입력한 문장 그대로 저장할까요? [y/N] › ");
    std::io::stdout()
        .flush()
        .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
    let mut confirmation = String::new();
    std::io::stdin()
        .read_line(&mut confirmation)
        .map_err(|error| MkoError::new("remember_input_failed", error.to_string()))?;
    if confirmation.trim() != "y" {
        println!("저장하지 않았습니다.");
        return Ok(());
    }
    let result = publish_quick_note_v2(
        &repository,
        &prepared,
        &prepared.confirmation_phrase,
        &SystemClock,
    )?;
    println!(
        "{}",
        match result.outcome {
            QuickNotePublicationOutcomeV2::Created => "메모를 저장했습니다.",
            QuickNotePublicationOutcomeV2::Existing => "같은 메모가 이미 저장되어 있습니다.",
        }
    );
    Ok(())
}

fn confirm_perspectives(arguments: PerspectiveArgs) -> Result<(), MkoError> {
    require_perspective_tty()?;
    let repository = setup_repository(arguments.repo)?;
    if detect_repository_generation(&repository)? != RepositoryGeneration::V3 {
        return Err(MkoError::new(
            "perspective_v3_required",
            "관점 확인은 v3 Personal KB에서 사용할 수 있습니다",
        ));
    }
    confirm_perspectives_for_id(
        &repository,
        &arguments.knowledge_id,
        arguments.perspectives.into_iter().map(Into::into).collect(),
    )
}

fn require_perspective_tty() -> Result<(), MkoError> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(MkoError::new(
            "perspective_tty_required",
            "관점 확인은 정확한 revision과 효과를 표시하는 실제 터미널에서만 가능합니다",
        ));
    }
    Ok(())
}

fn confirm_perspectives_for_id(
    repository: &Path,
    knowledge_id: &str,
    perspectives: Vec<PerspectiveV2>,
) -> Result<(), MkoError> {
    require_perspective_tty()?;
    let prepared = prepare_perspective_confirmation_v2(repository, knowledge_id, perspectives)
        .map_err(|error| {
            if error.code() == "high_risk_knowledge_incomplete" {
                MkoError::new(
                    error.code(),
                    "투자 관점에는 반론과 열린 질문이 모두 필요합니다. 먼저 수정본을 만든 뒤 다시 선택하세요",
                )
            } else {
                error
            }
        })?;
    std::io::stdout()
        .write_all(&prepared.confirmation_card)
        .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
    print!("\n표시한 관점으로 새 검토 revision을 만들까요? [y/N] › ");
    std::io::stdout()
        .flush()
        .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
    let mut confirmation = String::new();
    std::io::stdin()
        .read_line(&mut confirmation)
        .map_err(|error| MkoError::new("perspective_input_failed", error.to_string()))?;
    if confirmation.trim() != "y" {
        println!("변경하지 않았습니다.");
        return Ok(());
    }
    let result = publish_perspective_confirmation_v2(
        repository,
        &prepared,
        &prepared.confirmation_phrase,
        &SystemClock,
    )?;
    println!(
        "{}",
        match result.outcome {
            RecordWriteOutcomeV2::Created | RecordWriteOutcomeV2::Existing => {
                "관점이 이미 현재 revision에 반영되어 있습니다."
            }
            RecordWriteOutcomeV2::Replaced => {
                "관점을 반영한 새 revision을 만들었습니다. 다시 검토가 필요합니다."
            }
        }
    );
    Ok(())
}

fn resurface(repository: &Path) -> Result<(), MkoError> {
    require_perspective_tty()?;
    println!("관점으로 좁혀볼 수 있습니다.");
    println!("[Enter] 전체 · [1] 생활 · [2] 학습 · [3] 기술 · [4] 프로젝트 · [5] 투자");
    print!("관점 필터 › ");
    std::io::stdout()
        .flush()
        .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
    let mut filter = String::new();
    std::io::stdin()
        .read_line(&mut filter)
        .map_err(|error| MkoError::new("perspective_input_failed", error.to_string()))?;
    let perspective = match filter.trim() {
        "" | "0" => None,
        "1" => Some(PerspectiveV2::Life),
        "2" => Some(PerspectiveV2::Learning),
        "3" => Some(PerspectiveV2::Technical),
        "4" => Some(PerspectiveV2::Project),
        "5" => Some(PerspectiveV2::Investment),
        _ => {
            return Err(MkoError::new(
                "perspective_selection_invalid",
                "전체는 Enter, 관점은 1부터 5 사이의 번호를 입력하세요",
            ));
        }
    };
    let items = resurface_knowledge_by_perspective_v2(repository, perspective, 5)?;
    if items.is_empty() {
        println!("이 관점으로 다시 볼 지식이 아직 없습니다.");
        return Ok(());
    }
    println!();
    println!("다시 볼 지식");
    for (index, item) in items.iter().enumerate() {
        println!(
            "{}. {}{}{}",
            index + 1,
            item.title,
            if item.review_state == ResurfacedKnowledgeStateV2::Deferred {
                " · 나중에 보기"
            } else {
                ""
            },
            if item.has_open_questions {
                " · 열린 질문 있음"
            } else {
                ""
            }
        );
        println!("   {}", compact_excerpt(&item.synthesis, 140));
        if !item.perspectives.is_empty() {
            println!(
                "   관점: {}",
                item.perspectives
                    .iter()
                    .map(perspective_label)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    println!();
    print!("자세히 볼 지식 번호 [Enter: 닫기] › ");
    std::io::stdout()
        .flush()
        .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
    let mut selected_item = String::new();
    std::io::stdin()
        .read_line(&mut selected_item)
        .map_err(|error| MkoError::new("perspective_input_failed", error.to_string()))?;
    if matches!(selected_item.trim(), "" | "q" | "Q") {
        return Ok(());
    }
    let selected_index = selected_item
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|index| (1..=items.len()).contains(index))
        .ok_or_else(|| {
            MkoError::new(
                "perspective_selection_invalid",
                "표시된 지식 번호를 입력하세요",
            )
        })?;
    let selected = &items[selected_index - 1];
    println!();
    println!("{}", selected.title);
    println!(
        "{} · 검토 {} · 마지막 열람 {}",
        match selected.review_state {
            ResurfacedKnowledgeStateV2::Deferred => "나중에 보기",
            ResurfacedKnowledgeStateV2::Approved => "승인됨",
        },
        selected.reviewed_at.format("%Y-%m-%d"),
        selected
            .last_opened_at
            .map(|opened_at| opened_at.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "처음".to_owned())
    );
    println!();
    println!("{}", selected.synthesis);
    record_resurfaced_knowledge_open_v2(
        repository,
        &selected.knowledge_id,
        &selected.current_revision,
        &SystemClock,
    )?;
    println!();
    print!("[p] 관점 정하기 · [Enter] 닫기 › ");
    std::io::stdout()
        .flush()
        .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
    let mut detail_action = String::new();
    std::io::stdin()
        .read_line(&mut detail_action)
        .map_err(|error| MkoError::new("perspective_input_failed", error.to_string()))?;
    if matches!(detail_action.trim(), "" | "q" | "Q") {
        return Ok(());
    }
    if !detail_action.trim().eq_ignore_ascii_case("p") {
        return Err(MkoError::new(
            "resurface_action_invalid",
            "관점을 정하려면 p를 입력하고, 닫으려면 Enter를 누르세요",
        ));
    }
    println!();
    println!("{}의 관점을 선택하세요.", selected.title);
    for (index, perspective) in PerspectiveV2::all().iter().enumerate() {
        println!(
            "[{}] {}{}",
            index + 1,
            perspective_label(perspective),
            if selected.perspectives.contains(perspective) {
                " · 현재"
            } else {
                ""
            }
        );
    }
    print!("여러 개는 쉼표로 구분 [Enter: 취소] › ");
    std::io::stdout()
        .flush()
        .map_err(|error| MkoError::new("output_failed", error.to_string()))?;
    let mut selected_perspectives = String::new();
    std::io::stdin()
        .read_line(&mut selected_perspectives)
        .map_err(|error| MkoError::new("perspective_input_failed", error.to_string()))?;
    if selected_perspectives.trim().is_empty() {
        println!("변경하지 않았습니다.");
        return Ok(());
    }
    let perspectives = parse_perspective_numbers(&selected_perspectives)?;
    confirm_perspectives_for_id(repository, &selected.knowledge_id, perspectives)
}

fn parse_perspective_numbers(input: &str) -> Result<Vec<PerspectiveV2>, MkoError> {
    let mut perspectives = input
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .filter(|value| !value.is_empty())
        .map(|value| match value {
            "1" => Ok(PerspectiveV2::Life),
            "2" => Ok(PerspectiveV2::Learning),
            "3" => Ok(PerspectiveV2::Technical),
            "4" => Ok(PerspectiveV2::Project),
            "5" => Ok(PerspectiveV2::Investment),
            _ => Err(MkoError::new(
                "perspective_selection_invalid",
                "관점은 1부터 5 사이의 번호를 쉼표로 구분해 입력하세요",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    perspectives.sort();
    perspectives.dedup();
    if perspectives.is_empty() {
        return Err(MkoError::new(
            "perspective_selection_invalid",
            "관점을 하나 이상 선택하세요",
        ));
    }
    Ok(perspectives)
}

fn perspective_label(perspective: &PerspectiveV2) -> &'static str {
    match perspective {
        PerspectiveV2::Life => "생활",
        PerspectiveV2::Learning => "학습",
        PerspectiveV2::Technical => "기술",
        PerspectiveV2::Project => "프로젝트",
        PerspectiveV2::Investment => "투자",
    }
}

fn compact_excerpt(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let excerpt = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    }
}

fn run(cli: Cli) -> Result<Exit, MkoError> {
    match cli.command {
        None => home().map(|_| Exit::Success),
        Some(Command::Handshake(arguments)) => handshake(arguments).map(|_| Exit::Success),
        Some(Command::Schema {
            command: SchemaCommand::List(arguments),
        }) => schema_list(arguments).map(|_| Exit::Success),
        Some(Command::Schema {
            command: SchemaCommand::Show(arguments),
        }) => schema_show(arguments).map(|_| Exit::Success),
        Some(Command::Setup(arguments)) => setup(arguments).map(|_| Exit::Success),
        Some(Command::Add(arguments)) => add(arguments).map(|_| Exit::Success),
        Some(Command::Find(arguments)) => find(arguments).map(|_| Exit::Success),
        Some(Command::Remember(arguments)) => remember(arguments).map(|_| Exit::Success),
        Some(Command::Perspective(arguments)) => {
            confirm_perspectives(arguments).map(|_| Exit::Success)
        }
        Some(Command::Asset {
            command: AssetCommand::Capture(arguments),
        }) => capture(arguments).map(|_| Exit::Success),
        Some(Command::Asset {
            command: AssetCommand::Inspect(arguments),
        }) => inspect(arguments).map(|_| Exit::Success),
        Some(Command::Asset {
            command: AssetCommand::AcceptChange(arguments),
        }) => accept_change(arguments).map(|_| Exit::Success),
        Some(Command::Asset {
            command: AssetCommand::RepairLineage(arguments),
        }) => repair_asset_lineage(arguments).map(|_| Exit::Success),
        Some(Command::Check(arguments)) => check(arguments),
        Some(Command::Doctor(arguments)) => doctor(arguments).map(|_| Exit::Success),
        Some(Command::Inbox(arguments)) => inbox(arguments).map(|_| Exit::Success),
        Some(Command::Status(arguments)) => status(arguments).map(|_| Exit::Success),
        Some(Command::Queue(arguments)) => queue_v2(arguments).map(|_| Exit::Success),
        Some(Command::Show(arguments)) => show_v2(arguments).map(|_| Exit::Success),
        Some(Command::ReviewOpen(arguments)) => review_open_v2(arguments).map(|_| Exit::Success),
        Some(Command::ReviewFeedback(arguments)) => {
            review_feedback_v2(arguments).map(|_| Exit::Success)
        }
        Some(Command::Review(arguments)) => review(arguments).map(|_| Exit::Success),
        Some(Command::Dashboard(arguments)) => dashboard(arguments).map(|_| Exit::Success),
        Some(Command::Knowledge {
            command: KnowledgeCommand::Write(arguments),
        }) => knowledge_write(arguments).map(|_| Exit::Success),
        Some(Command::Knowledge {
            command: KnowledgeCommand::Review(arguments),
        }) => knowledge_review(arguments).map(|_| Exit::Success),
        Some(Command::Knowledge {
            command: KnowledgeCommand::Search(arguments),
        }) => knowledge_search(arguments).map(|_| Exit::Success),
        Some(Command::Knowledge {
            command: KnowledgeCommand::Show(arguments),
        }) => knowledge_show(arguments).map(|_| Exit::Success),
        Some(Command::Knowledge {
            command: KnowledgeCommand::List(arguments),
        }) => knowledge_list(arguments).map(|_| Exit::Success),
        Some(Command::Source {
            command: SourceCommand::Prepare(arguments),
        }) => prepare(arguments).map(|_| Exit::Success),
        Some(Command::Source {
            command: SourceCommand::WriteDraft(arguments),
        }) => write_draft(arguments).map(|_| Exit::Success),
        Some(Command::Source {
            command: SourceCommand::RepairState(arguments),
        }) => repair_source(arguments).map(|_| Exit::Success),
        Some(Command::ExtractPdf) => extract_pdf().map(|_| Exit::Success),
        Some(Command::Human {
            command: HumanCommand::ApproveSource(arguments),
        }) => approve(arguments).map(|_| Exit::Success),
        Some(Command::Hooks {
            command: HooksCommand::Install(arguments),
        }) => install_hook(arguments).map(|_| Exit::Success),
    }
}

// The handshake is deliberately context-free: it must verify a fresh install
// before `mko setup` has ever run, so it never resolves a repository.
fn handshake(arguments: HandshakeArgs) -> Result<(), MkoError> {
    if arguments.format == OutputFormat::JsonV1 {
        return Err(MkoError::new(
            "format_unsupported",
            "mko handshake supports human or json-v2 output",
        ));
    }
    mko_core::version::verify_skill_version(&arguments.skill_version)?;
    match arguments.format {
        OutputFormat::JsonV2 => emit_json_v2(JsonV2Success::handshake(HandshakeDataV2 {
            cli_version: mko_core::version::PRODUCT_VERSION.into(),
            skill_version: arguments.skill_version,
        })),
        _ => {
            println!(
                "CLI {}와 Skill {}이 같은 계약입니다.",
                mko_core::version::PRODUCT_VERSION,
                arguments.skill_version
            );
            Ok(())
        }
    }
}

// Like the handshake, the schema surface is context-free: an installed CLI
// serves its own embedded contracts so the Skill never needs the repository
// checkout the schemas were authored in.
fn schema_list(arguments: SchemaListArgs) -> Result<(), MkoError> {
    if arguments.format == OutputFormat::JsonV1 {
        return Err(MkoError::new(
            "format_unsupported",
            "mko schema list supports human or json-v2 output",
        ));
    }
    let data = mko_core::schema_v2::list_schemas_v2();
    match arguments.format {
        OutputFormat::JsonV2 => emit_json_v2(JsonV2Success::schema_list(data)),
        _ => {
            for schema in data.schemas {
                println!("{} — {}", schema.name, schema.purpose);
            }
            Ok(())
        }
    }
}

fn schema_show(arguments: SchemaShowArgs) -> Result<(), MkoError> {
    if arguments.format == OutputFormat::JsonV1 {
        return Err(MkoError::new(
            "format_unsupported",
            "mko schema show supports human or json-v2 output",
        ));
    }
    let data = mko_core::schema_v2::show_schema_v2(&arguments.name)?;
    match arguments.format {
        OutputFormat::JsonV2 => emit_json_v2(JsonV2Success::schema_show(data)),
        _ => {
            println!("{} — {}", data.name, data.purpose);
            println!("schema:");
            println!("{}", pretty_json(&data.schema)?);
            println!("example:");
            println!("{}", pretty_json(&data.example)?);
            Ok(())
        }
    }
}

fn pretty_json(value: &serde_json::Value) -> Result<String, MkoError> {
    serde_json::to_string_pretty(value)
        .map_err(|error| MkoError::new("json_output_failed", error.to_string()))
}

fn add(arguments: AddArgs) -> Result<(), MkoError> {
    let context = resolve_context(None).map_err(|error| {
        if arguments.inbox {
            map_add_inbox_error(error)
        } else {
            error
        }
    })?;
    if crate::cli_v2::is_v2_repository(&context.repository_root)? {
        return add_v2(arguments, &context);
    }
    let input = if arguments.inbox {
        AddInput::InboxScan
    } else {
        AddInput::File(arguments.file.unwrap())
    };
    let request = AddRequest::new(context, input)
        .with_temporary_source(arguments.temporary_source)
        .with_backup_attestation(if arguments.verified_backup {
            BackupAttestation::UserVerified
        } else {
            BackupAttestation::OutsideOriginalRetained
        });
    let result =
        add_input(request, &SystemClock, &MonotonicElapsedClock::start()).map_err(|error| {
            if arguments.inbox {
                map_add_inbox_error(error)
            } else {
                error
            }
        })?;
    match result {
        AddRunResult::Single(result) => {
            if arguments.format == OutputFormat::JsonV1 {
                emit_json_v1(JsonV1Success::Add {
                    schema_version: 1,
                    result: SuccessResult::Ok,
                    data: AddPayload::Single(AddData {
                        add_outcome: result.add_outcome,
                        import_outcome: result.import_outcome,
                        repository: result.repository.display().to_string(),
                        asset_id: result.asset_id,
                        registry_path: result.registry_path,
                        provider_locator: result.provider_locator,
                    }),
                })
            } else {
                println!("{} {}", result.add_outcome_string(), result.asset_id);
                Ok(())
            }
        }
        AddRunResult::Batch(result) => {
            if arguments.format == OutputFormat::JsonV1 {
                emit_json_v1(JsonV1Success::Add {
                    schema_version: 1,
                    result: SuccessResult::Ok,
                    data: AddPayload::Batch(batch_add_data(result)),
                })
            } else {
                let blocked = result
                    .items
                    .iter()
                    .filter(|item| item.error.is_some())
                    .count();
                let registered = result
                    .items
                    .iter()
                    .filter(|item| item.add_outcome.is_some())
                    .count();
                println!("등록됨 {registered} · 확인 필요 {blocked}");
                if result.remaining > 0 {
                    println!("나머지 {}개는 다음 실행에서 처리합니다.", result.remaining);
                }
                Ok(())
            }
        }
    }
}

fn add_v2(arguments: AddArgs, context: &ResolvedPersonalContext) -> Result<(), MkoError> {
    if arguments.inbox {
        let result = register_inbox_pdf_assets_v2(
            RegisterInboxAssetsRequestV2 {
                repository_root: &context.repository_root,
                provider_root: &context.provider_root,
                hydration_confirmation: if arguments.confirm_download {
                    HydrationConfirmationV2::Confirmed
                } else {
                    HydrationConfirmationV2::NotConfirmed
                },
            },
            &MonotonicElapsedClock::start(),
        )?;
        let items = result
            .items
            .into_iter()
            .map(|item| {
                let (asset_id, outcome) = match item.registration {
                    Some(registration) => (
                        Some(registration.asset.id),
                        Some(match registration.outcome {
                            AssetRegistrationOutcomeV2::Created => AddOutcomeV2::Created,
                            AssetRegistrationOutcomeV2::Existing => AddOutcomeV2::Existing,
                        }),
                    ),
                    None => (None, None),
                };
                let error = item.error.map(|error| AddBatchItemErrorV2 {
                    code: error.code().into(),
                    message: error.message().into(),
                    next_action: json_v2_next_action(error.code()),
                });
                AddBatchItemV2 {
                    logical_locator: item.logical_locator,
                    asset_id,
                    outcome,
                    error,
                }
            })
            .collect::<Vec<_>>();
        let warnings = result
            .warnings
            .into_iter()
            .map(|warning| AddBatchWarningV2 {
                code: warning.code,
                message: warning.message,
                logical_locator: warning.provider_locator,
            })
            .collect::<Vec<_>>();
        let batch = AddBatchDataV2 {
            items,
            scan_complete: result.scan_complete,
            remaining: result.remaining,
            warnings,
        };
        return match arguments.format {
            OutputFormat::JsonV2 => {
                crate::output::emit_json_v2(JsonV2Success::add(AddDataV2::Batch(batch)))
            }
            OutputFormat::Human => {
                let created = batch
                    .items
                    .iter()
                    .filter(|item| item.outcome == Some(AddOutcomeV2::Created))
                    .count();
                let existing = batch
                    .items
                    .iter()
                    .filter(|item| item.outcome == Some(AddOutcomeV2::Existing))
                    .count();
                let blocked = batch
                    .items
                    .iter()
                    .filter(|item| item.error.is_some())
                    .count();
                println!("새 등록 {created} · 기존 {existing} · 확인 필요 {blocked}");
                if !batch.scan_complete || batch.remaining > 0 {
                    println!("검색 미완료 · 다음 실행 대기 {}개 이상", batch.remaining);
                }
                for item in &batch.items {
                    if let Some(error) = &item.error {
                        println!("- {}: {}", item.logical_locator, error.message);
                    }
                }
                Ok(())
            }
            OutputFormat::JsonV1 => Err(MkoError::new(
                "format_unsupported",
                "a v0.3 KB requires json-v2 output",
            )),
        };
    }
    if arguments.temporary_source || arguments.verified_backup {
        return Err(MkoError::new(
            "option_unsupported",
            "v0.3 registration preserves Inbox files and does not use legacy backup options",
        ));
    }
    let file = arguments
        .file
        .as_deref()
        .ok_or_else(|| MkoError::new("asset_path_required", "pass one PDF path"))?;
    let logical_locator = provider_logical_locator(&context.provider_root, file)?;
    let result = register_pdf_asset_v2(RegisterAssetRequestV2 {
        repository_root: &context.repository_root,
        provider_root: &context.provider_root,
        logical_locator: &logical_locator,
        hydration_confirmation: if arguments.confirm_download {
            HydrationConfirmationV2::Confirmed
        } else {
            HydrationConfirmationV2::NotConfirmed
        },
    })?;
    match arguments.format {
        OutputFormat::Human => {
            let outcome = match result.outcome {
                AssetRegistrationOutcomeV2::Created => "등록 완료",
                AssetRegistrationOutcomeV2::Existing => "이미 등록됨",
            };
            println!("{outcome}: {}", result.asset.id);
            println!("다음: 이 PDF를 요약해 달라고 요청하세요.");
            Ok(())
        }
        OutputFormat::JsonV2 => {
            crate::output::emit_json_v2(JsonV2Success::add(AddDataV2::Single(AddSingleDataV2 {
                asset_id: result.asset.id,
                outcome: match result.outcome {
                    AssetRegistrationOutcomeV2::Created => AddOutcomeV2::Created,
                    AssetRegistrationOutcomeV2::Existing => AddOutcomeV2::Existing,
                },
                registry_path: result.registry_path.display().to_string(),
                logical_locator,
            })))
        }
        OutputFormat::JsonV1 => Err(MkoError::new(
            "format_unsupported",
            "a v0.3 KB requires json-v2 output",
        )),
    }
}

fn provider_logical_locator(provider_root: &Path, file: &Path) -> Result<String, MkoError> {
    let provider_root = provider_root
        .canonicalize()
        .map_err(|error| MkoError::new("provider_root_invalid", error.to_string()))?;
    let candidate = if file.is_absolute() {
        file.to_path_buf()
    } else {
        let provider_candidate = provider_root.join(file);
        if provider_candidate.exists() {
            provider_candidate
        } else {
            std::env::current_dir()
                .map_err(|error| MkoError::new("current_directory_unavailable", error.to_string()))?
                .join(file)
        }
    };
    let candidate = candidate
        .canonicalize()
        .map_err(|error| MkoError::new("asset_not_found", error.to_string()))?;
    let relative = candidate.strip_prefix(&provider_root).map_err(|_| {
        MkoError::new(
            "asset_outside_inbox",
            "move or copy the PDF into the configured Personal Inbox, then run mko add again",
        )
    })?;
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(value) => components.push(
                value
                    .to_str()
                    .ok_or_else(|| MkoError::new("asset_path_invalid", "PDF path must be UTF-8"))?,
            ),
            _ => {
                return Err(MkoError::new(
                    "asset_path_invalid",
                    "PDF path must be a portable relative path",
                ));
            }
        }
    }
    if components.is_empty() {
        return Err(MkoError::new("asset_path_invalid", "PDF path is empty"));
    }
    Ok(components.join("/"))
}

fn prepare(arguments: PrepareArgs) -> Result<(), MkoError> {
    if arguments.format == Some(OutputFormat::JsonV2) {
        let context = resolve_context(arguments.repo.clone())?;
        return crate::cli_v2::prepare_source_json_v2(
            &context.repository_root,
            &context.provider_root,
            &arguments.asset_id,
            arguments.confirm_download,
            &worker_executable()?,
        );
    }
    if !format_is_json_v1(arguments.format) {
        return prepare_legacy(arguments);
    }
    let context = resolve_context(arguments.repo)?;
    let output = arguments.output.as_ref().ok_or_else(|| {
        MkoError::new(
            "runtime_output_required",
            "json-v1 source preparation requires --output",
        )
    })?;
    let runtime_output =
        normalized_runtime_output(&context.repository_root, &arguments.asset_id, output)?;
    let request = PrepareRequest::new(
        &context.repository_root,
        &arguments.asset_id,
        &runtime_output,
    )
    .with_resolved_context(context)
    .with_clear_stale_lock(arguments.clear_stale_lock);
    let bundle = prepare_source(request, &worker_executable()?)?;
    if format_is_json_v1(arguments.format) {
        emit_json_v1(JsonV1Success::SourcePrepare {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: PrepareData {
                asset_id: bundle.asset_id,
                source_id: bundle.source_id,
                bundle_path: runtime_output
                    .canonicalize()
                    .map_err(|error| MkoError::new("runtime_output_invalid", error.to_string()))?
                    .display()
                    .to_string(),
            },
        })
    } else {
        println!("prepared {} {}", bundle.asset_id, bundle.source_id);
        Ok(())
    }
}

fn prepare_legacy(arguments: PrepareArgs) -> Result<(), MkoError> {
    let output = arguments.output.as_ref().ok_or_else(|| {
        MkoError::new(
            "runtime_output_required",
            "legacy source preparation requires --output",
        )
    })?;
    let mut request = PrepareRequest::new(
        arguments.repo.as_ref().unwrap(),
        &arguments.asset_id,
        output,
    )
    .with_clear_stale_lock(arguments.clear_stale_lock);
    if let Some(config) = arguments.local_config {
        request = request.with_local_config(config);
    }
    let bundle = prepare_source(request, &worker_executable()?)?;
    println!("prepared {} {}", bundle.asset_id, bundle.source_id);
    Ok(())
}

fn write_draft(arguments: WriteDraftArgs) -> Result<(), MkoError> {
    if arguments.format == Some(OutputFormat::JsonV2) {
        let context = resolve_context(arguments.repo.clone())?;
        return crate::cli_v2::write_source_json_v2(
            &context.repository_root,
            &arguments.bundle,
            &arguments.response,
            arguments.expected_revision.as_deref(),
            &SystemClock,
        );
    }
    let json_v1 = format_is_json_v1(arguments.format);
    let repository = if json_v1 {
        resolve_context(arguments.repo.clone())?.repository_root
    } else {
        arguments.repo.clone().unwrap()
    };
    let response = std::fs::read(&arguments.response).map_err(|error| {
        MkoError::new(
            "semantic_response_unreadable",
            format!("cannot read {}: {error}", arguments.response.display()),
        )
    })?;
    let result = write_source_draft(
        WriteSourceRequest::new(&repository, &arguments.bundle, response)
            .with_slug(arguments.slug)
            .with_replace_pending(arguments.replace_pending)
            .with_clear_stale_lock(arguments.clear_stale_lock),
    )?;
    if json_v1 {
        emit_json_v1(JsonV1Success::SourceWriteDraft {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: WriteDraftData {
                draft_outcome: draft_outcome(&result.result)?,
                source_id: result.source_id,
                source_path: result.source_path,
                content_revision: result.content_revision,
            },
        })
    } else if arguments.json {
        emit_json_value(
            &serde_json::json!({"result":result.result,"source_id":result.source_id,"source_path":result.source_path,"content_revision":result.content_revision}),
        )
    } else {
        println!(
            "{} {} {} {}",
            result.result, result.source_id, result.source_path, result.content_revision
        );
        Ok(())
    }
}

fn check(arguments: CheckArgs) -> Result<Exit, MkoError> {
    let json_v1 = format_is_json_v1(arguments.format);
    let repository = if json_v1 {
        resolve_context(arguments.repo.clone())?.repository_root
    } else {
        arguments.repo.clone().unwrap()
    };
    let report = check_repository(CheckRequest::new(repository).with_staged(arguments.staged))?;
    if json_v1 {
        emit_json_v1(JsonV1Success::Check {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: check_data(&report),
        })?;
    } else if arguments.json {
        emit_encoded_json(
            &serde_json::to_string(&report)
                .map_err(|error| MkoError::new("check_failed", error.to_string()))?,
        )?;
    } else if report.is_ok() {
        println!("ok");
    } else {
        for issue in &report.issues {
            println!(
                "{} {}: {}",
                issue.code,
                issue.path.as_deref().unwrap_or("-"),
                issue.message
            );
            if let Some(action) = &issue.safe_action {
                println!("  repair: {action}");
            }
        }
    }
    Ok(if report.is_ok() {
        Exit::Success
    } else {
        Exit::ValidationFailed
    })
}

// Recovery for the owner who stopped mid-operation: Core only takes over a
// lock whose owning process is gone, so this can never interrupt live work.
fn clear_stale_lock(repo: Option<PathBuf>) -> Result<(), MkoError> {
    let repository = setup_repository(repo)?;
    if mko_core::lock::clear_stale_repository_lock(&repository, &SystemClock)? {
        println!("중단된 작업이 남긴 잠금을 해제했습니다. 이제 다시 사용할 수 있습니다.");
    } else {
        println!("해제할 잠금이 없습니다.");
        println!("다른 My Knowledge OS 작업이 실행 중이라면 끝난 뒤 다시 시도하세요.");
    }
    Ok(())
}

// A check's recovery hint and the report's overall next step both have to
// arrive as the same typed vocabulary the rest of the v2 surface uses.
fn recovery_next_action_v2(kind: RecoveryKind) -> NextActionV2 {
    match kind {
        RecoveryKind::Configure => NextActionV2::Configure,
        RecoveryKind::Hydrate => NextActionV2::Hydrate,
        RecoveryKind::VerifyBackup => NextActionV2::Add,
        RecoveryKind::FixPermissions | RecoveryKind::ResolveHookConflict => NextActionV2::Repair,
        RecoveryKind::Retry => NextActionV2::Retry,
        RecoveryKind::Repair => NextActionV2::Repair,
    }
}

fn doctor_next_action_v2(next_action: &NextAction) -> NextActionV2 {
    match next_action {
        NextAction::None => NextActionV2::None,
        NextAction::Configure => NextActionV2::Configure,
        NextAction::Hydrate => NextActionV2::Hydrate,
        NextAction::Add => NextActionV2::Add,
        NextAction::Prepare => NextActionV2::Prepare,
        NextAction::WriteDraft => NextActionV2::WriteSource,
        NextAction::Review => NextActionV2::Review,
        NextAction::Repair => NextActionV2::Repair,
        NextAction::Retry => NextActionV2::Retry,
    }
}

fn doctor(arguments: DoctorArgs) -> Result<(), MkoError> {
    if arguments.clear_stale_lock {
        return clear_stale_lock(arguments.repo);
    }
    let request = match arguments.repo {
        Some(repository) => DoctorRequest::new().with_repository(repository),
        None => DoctorRequest::new(),
    };
    let environment = SystemDoctorEnvironment::default();
    let report = diagnose(request, &environment);
    if arguments.format == OutputFormat::JsonV2 {
        return emit_json_v2(JsonV2Success::doctor(DoctorDataV2 {
            healthy: report.healthy,
            checks: report
                .checks
                .into_iter()
                .map(|check| DoctorCheckDataV2 {
                    code: check.code,
                    status: match check.status {
                        DoctorCheckStatus::Healthy => DoctorCheckStatusV2::Healthy,
                        DoctorCheckStatus::Warning => DoctorCheckStatusV2::Warning,
                        DoctorCheckStatus::Blocked => DoctorCheckStatusV2::Blocked,
                    },
                    message: check.message,
                    path: check.path.map(|path| path.display().to_string()),
                    next_action: check.recovery.map(recovery_next_action_v2),
                })
                .collect(),
            next_action: doctor_next_action_v2(&report.next_action),
        }));
    }
    if arguments.format == OutputFormat::JsonV1 {
        emit_json_v1(JsonV1Success::Doctor {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: DoctorData {
                healthy: report.healthy,
                checks: report
                    .checks
                    .into_iter()
                    .map(|check| DoctorCheckData {
                        code: check.code,
                        status: check.status,
                        message: check.message,
                        path: check.path.map(|path| path.display().to_string()),
                        recovery: check.recovery.map(|kind| Recovery { kind }),
                    })
                    .collect(),
                next_action: report.next_action,
            },
        })
    } else {
        emit_doctor_human(&report)
    }
}

fn inbox(arguments: InboxArgs) -> Result<(), MkoError> {
    let context = resolve_catalog_context(arguments.repo, JsonV1Command::Inbox)?;
    let report = scan_inbox(
        InboxScanRequest::new(&context.repository_root, &context.provider_root),
        &MonotonicElapsedClock::start(),
    )
    .map_err(map_inbox_error)?;
    if arguments.format == OutputFormat::JsonV1 {
        emit_json_v1(JsonV1Success::Inbox {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: report.into(),
        })
    } else {
        emit_inbox_human(&report)
    }
}

fn status(arguments: StatusArgs) -> Result<(), MkoError> {
    let context = resolve_catalog_context(arguments.repo, JsonV1Command::Status)?;
    let inbox = scan_inbox(
        InboxScanRequest::new(&context.repository_root, &context.provider_root),
        &MonotonicElapsedClock::start(),
    )
    .map_err(map_status_error)?;
    let report = status_from_inbox(&inbox);
    if arguments.format == OutputFormat::JsonV1 {
        emit_json_v1(JsonV1Success::Status {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: report.into(),
        })
    } else {
        emit_status_human(&report)
    }
}

fn review(arguments: ReviewArgs) -> Result<(), MkoError> {
    let repository = setup_repository(arguments.repo)?;
    if crate::cli_v2::is_v2_repository(&repository)? {
        return crate::cli_v2::review(&repository, arguments.stable_id.as_deref(), &SystemClock);
    }
    match review_pending(&repository)? {
        ReviewOutcome::Deferred => println!("deferred"),
        ReviewOutcome::Approved(result) => {
            println!("approved {} {}", result.source_id, result.revision)
        }
    }
    Ok(())
}

fn queue_v2(arguments: QueueArgs) -> Result<(), MkoError> {
    let repository = setup_repository(arguments.repo)?;
    match arguments.format {
        OutputFormat::Human => crate::cli_v2::queue(&repository),
        OutputFormat::JsonV2 => crate::cli_v2::queue_json_v2(&repository),
        OutputFormat::JsonV1 => Err(MkoError::new(
            "format_unsupported",
            "mko queue supports human or json-v2 output",
        )),
    }
}

fn show_v2(arguments: ShowArgs) -> Result<(), MkoError> {
    let repository = setup_repository(arguments.repo)?;
    match arguments.format {
        OutputFormat::Human => crate::cli_v2::show(&repository, &arguments.stable_id),
        OutputFormat::JsonV2 => crate::cli_v2::show_json_v2(&repository, &arguments.stable_id),
        OutputFormat::JsonV1 => Err(MkoError::new(
            "format_unsupported",
            "mko show supports human or json-v2 output",
        )),
    }
}

fn review_open_v2(arguments: ReviewOpenArgs) -> Result<(), MkoError> {
    if arguments.format != OutputFormat::JsonV2 {
        return Err(MkoError::new(
            "format_unsupported",
            "mko review-open requires json-v2 output",
        ));
    }
    let repository = setup_repository(arguments.repo)?;
    crate::cli_v2::review_open_json_v2(&repository, &arguments.stable_id, &SystemClock)
}

fn review_feedback_v2(arguments: ReviewFeedbackArgs) -> Result<(), MkoError> {
    if arguments.format != OutputFormat::JsonV2 {
        return Err(MkoError::new(
            "format_unsupported",
            "mko review-feedback requires json-v2 output",
        ));
    }
    let repository = setup_repository(arguments.repo)?;
    crate::cli_v2::review_feedback_json_v2(&repository, &arguments.input, &SystemClock)
}

fn knowledge_write(arguments: KnowledgeWriteArgs) -> Result<(), MkoError> {
    if arguments.format == Some(OutputFormat::JsonV2) {
        let context = resolve_context(arguments.repo.clone())?;
        return crate::cli_v2::write_knowledge_json_v2(
            &context.repository_root,
            &arguments.bundle,
            &arguments.response,
            arguments.expected_revision.as_deref(),
            &SystemClock,
        );
    }
    let json_v1 = format_is_json_v1(arguments.format);
    let repository = if json_v1 {
        resolve_context(arguments.repo.clone())?.repository_root
    } else {
        arguments.repo.clone().unwrap()
    };
    let bundle = if json_v1 {
        normalized_runtime_output(&repository, &arguments.asset_id, &arguments.bundle)?
    } else {
        arguments.bundle.clone()
    };
    let response = std::fs::read(&arguments.response).map_err(|error| {
        MkoError::new(
            "knowledge_response_unreadable",
            format!("cannot read {}: {error}", arguments.response.display()),
        )
    })?;
    let result = write_knowledge_note(
        WriteKnowledgeRequest::new(&repository, &arguments.asset_id, response)
            .with_bundle(bundle)
            .with_replace(arguments.replace),
    )?;
    if json_v1 {
        emit_json_v1(JsonV1Success::KnowledgeWrite {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: KnowledgeWriteData {
                write_outcome: knowledge_write_outcome(&result.result)?,
                asset_id: arguments.asset_id,
                knowledge_id: result.knowledge_id,
                knowledge_path: result.knowledge_path,
                content_revision: result.content_revision,
            },
        })
    } else {
        println!(
            "{} {} {} {}",
            result.result, arguments.asset_id, result.knowledge_path, result.content_revision
        );
        Ok(())
    }
}

fn knowledge_write_outcome(result: &str) -> Result<KnowledgeWriteOutcome, MkoError> {
    match result {
        "created" => Ok(KnowledgeWriteOutcome::Created),
        "existing" => Ok(KnowledgeWriteOutcome::Existing),
        "replaced" => Ok(KnowledgeWriteOutcome::Replaced),
        _ => Err(MkoError::new(
            "knowledge_write_result_invalid",
            "knowledge write returned an unknown result",
        )),
    }
}

fn knowledge_search(arguments: KnowledgeSearchArgs) -> Result<(), MkoError> {
    let json_v1 = format_is_json_v1(arguments.format);
    let repository = if json_v1 {
        resolve_context(arguments.repo.clone())?.repository_root
    } else {
        arguments.repo.clone().unwrap()
    };
    let query = KnowledgeSearchQuery {
        term: arguments.term.clone(),
        kind: arguments.kind.map(Into::into),
        tag: arguments.tag.clone(),
    };
    let matches = search_knowledge(&repository, &query)?;
    if json_v1 {
        emit_json_v1(JsonV1Success::KnowledgeSearch {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: KnowledgeSearchData {
                matches: matches.iter().map(concept_match_data).collect(),
            },
        })
    } else {
        for concept in &matches {
            println!(
                "{} {} {} {}",
                concept.asset_id,
                concept.title,
                concept.name,
                concept_kind_label(&concept.kind)
            );
        }
        Ok(())
    }
}

fn knowledge_show(arguments: KnowledgeShowArgs) -> Result<(), MkoError> {
    let json_v1 = format_is_json_v1(arguments.format);
    let repository = if json_v1 {
        resolve_context(arguments.repo.clone())?.repository_root
    } else {
        arguments.repo.clone().unwrap()
    };
    let mut notes = list_knowledge(&repository)?
        .into_iter()
        .filter(|item| item.asset_id == arguments.asset_id)
        .collect::<Vec<_>>();
    if notes.is_empty() {
        return Err(MkoError::new(
            "knowledge_not_found",
            "no knowledge note was found for that asset",
        ));
    }
    if notes.len() > 1 {
        return Err(MkoError::new(
            "knowledge_conflict",
            "multiple knowledge notes refer to that asset",
        ));
    }
    let note = notes.pop().expect("the non-empty length was checked");
    let review_status = match note.review_status {
        mko_core::knowledge::ReviewState::Unreviewed => KnowledgeReviewStatusData::Unreviewed,
        mko_core::knowledge::ReviewState::Reviewed => KnowledgeReviewStatusData::Reviewed,
    };
    if json_v1 {
        emit_json_v1(JsonV1Success::KnowledgeShow {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: KnowledgeShowData {
                asset_id: arguments.asset_id,
                title: note.title,
                knowledge_path: note.knowledge_path,
                review_status,
                knowledge_id: Some(note.knowledge_id),
                content_revision: Some(note.content_revision),
                concepts: note
                    .concepts
                    .iter()
                    .map(|concept| KnowledgeConceptSummary {
                        name: concept.name.clone(),
                        kind: concept.kind.clone(),
                        locator: concept.locator.clone(),
                    })
                    .collect(),
            },
        })
    } else {
        println!(
            "{} {} {}",
            arguments.asset_id, note.title, note.knowledge_path
        );
        for concept in &note.concepts {
            println!("- {} ({})", concept.name, concept_kind_label(&concept.kind));
        }
        Ok(())
    }
}

fn knowledge_list(arguments: KnowledgeListArgs) -> Result<(), MkoError> {
    let json_v1 = format_is_json_v1(arguments.format);
    let repository = if json_v1 {
        resolve_context(arguments.repo.clone())?.repository_root
    } else {
        arguments.repo.clone().unwrap()
    };
    let notes = list_knowledge(&repository)?;
    if json_v1 {
        emit_json_v1(JsonV1Success::KnowledgeList {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: KnowledgeListData {
                items: notes
                    .into_iter()
                    .map(|item| KnowledgePendingItemData {
                        knowledge_id: item.knowledge_id,
                        asset_id: item.asset_id,
                        title: item.title,
                        knowledge_path: item.knowledge_path,
                        content_revision: item.content_revision,
                    })
                    .collect(),
            },
        })
    } else {
        for item in &notes {
            println!(
                "{} {} {} {}",
                item.asset_id, item.title, item.knowledge_path, item.content_revision
            );
        }
        Ok(())
    }
}

fn knowledge_review(arguments: KnowledgeReviewArgs) -> Result<(), MkoError> {
    let json_v1 = format_is_json_v1(arguments.format);
    let repository = if json_v1 {
        resolve_context(arguments.repo.clone())?.repository_root
    } else {
        arguments.repo.clone().unwrap()
    };
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(MkoError::new(
            "human_confirmation_required",
            "knowledge review requires an interactive terminal",
        ));
    }
    let pending = list_unreviewed_knowledge(&repository)?;
    let selected = match &arguments.asset_id {
        Some(asset_id) => pending
            .into_iter()
            .filter(|item| &item.asset_id == asset_id)
            .collect::<Vec<_>>(),
        None => pending,
    };
    if selected.is_empty() {
        return Err(MkoError::new(
            "knowledge_not_found",
            "no unreviewed knowledge note is available for review",
        ));
    }
    let mut items = Vec::new();
    for note in selected {
        if json_v1 {
            eprintln!("{}", note.rendered_markdown);
            eprint!("{} · {} — approve/defer: ", note.title, note.asset_id);
            std::io::stderr()
                .flush()
                .map_err(|error| MkoError::new("terminal_write_failed", error.to_string()))?;
        } else {
            println!("{}", note.rendered_markdown);
            print!("{} · {} — approve/defer: ", note.title, note.asset_id);
            std::io::stdout()
                .flush()
                .map_err(|error| MkoError::new("terminal_write_failed", error.to_string()))?;
        }
        let mut choice = String::new();
        std::io::stdin()
            .read_line(&mut choice)
            .map_err(|error| MkoError::new("terminal_read_failed", error.to_string()))?;
        let choice = choice.trim().to_lowercase();
        let decision = if choice == "approve" || choice == "y" || choice == "yes" {
            approve_knowledge(&repository, &note.knowledge_id, &note.content_revision)?;
            KnowledgeReviewDecision::Approved
        } else {
            KnowledgeReviewDecision::Deferred
        };
        if json_v1 {
            items.push(KnowledgeReviewItemData {
                knowledge_id: note.knowledge_id,
                asset_id: note.asset_id,
                title: note.title,
                decision,
            });
        } else {
            println!(
                "{} {} {}",
                match decision {
                    KnowledgeReviewDecision::Approved => "approved",
                    KnowledgeReviewDecision::Deferred => "deferred",
                },
                note.knowledge_id,
                note.asset_id,
            );
        }
    }
    if json_v1 {
        let remaining = list_unreviewed_knowledge(&repository)?.len() as u64;
        emit_json_v1(JsonV1Success::KnowledgeReview {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: KnowledgeReviewData {
                items,
                remaining_unreviewed: remaining,
            },
        })
    } else {
        Ok(())
    }
}

fn concept_match_data(concept: &ConceptMatch) -> ConceptMatchData {
    ConceptMatchData {
        asset_id: concept.asset_id.clone(),
        title: concept.title.clone(),
        name: concept.name.clone(),
        kind: concept.kind.clone(),
        locator: concept.locator.clone(),
        knowledge_path: concept.knowledge_path.clone(),
    }
}

fn concept_kind_label(kind: &ConceptKind) -> &'static str {
    match kind {
        ConceptKind::Definition => "definition",
        ConceptKind::Formula => "formula",
        ConceptKind::Concept => "concept",
        ConceptKind::Result => "result",
        ConceptKind::Theorem => "theorem",
    }
}

fn resolve_catalog_context(
    repository: Option<PathBuf>,
    command: JsonV1Command,
) -> Result<ResolvedPersonalContext, MkoError> {
    resolve_context(repository).map_err(|error| match command {
        JsonV1Command::Inbox
            if matches!(
                error.code(),
                "context_not_found" | "profile_missing" | "profile_invalid"
            ) =>
        {
            MkoError::new("inbox_unavailable", "The inbox is not configured.")
        }
        JsonV1Command::Status
            if matches!(
                error.code(),
                "context_not_found" | "profile_missing" | "profile_invalid"
            ) =>
        {
            MkoError::new(
                "repository_not_configured",
                "No default repository is configured.",
            )
        }
        _ => error,
    })
}

fn map_inbox_error(error: MkoError) -> MkoError {
    if matches!(
        error.code(),
        "provider_root_invalid" | "provider_scan_failed" | "repository_unreadable"
    ) {
        MkoError::new("inbox_unavailable", "The inbox could not be scanned.")
    } else {
        error
    }
}

fn map_add_inbox_error(error: MkoError) -> MkoError {
    if matches!(
        error.code(),
        "context_not_found"
            | "profile_missing"
            | "profile_invalid"
            | "provider_root_invalid"
            | "provider_inspection_failed"
            | "provider_scan_failed"
            | "repository_unreadable"
            | "repository_root_invalid"
    ) {
        MkoError::new("inbox_unavailable", "The inbox could not be scanned.")
    } else if matches!(
        error.code(),
        "scan_time_limit" | "scan_entry_limit" | "scan_byte_limit" | "scan_depth_limit"
    ) {
        MkoError::new(
            "provider_scan_incomplete",
            "The inbox scan was incomplete; retry after resolving its blockers.",
        )
    } else {
        error
    }
}

fn map_status_error(error: MkoError) -> MkoError {
    if error.code() == "repository_root_invalid" {
        MkoError::new("repository_unreadable", "The repository is not readable.")
    } else {
        error
    }
}

fn emit_inbox_human(report: &InboxScanResult) -> Result<(), MkoError> {
    if report.items.is_empty() {
        println!("Inbox에 표시할 PDF가 없습니다.");
    } else {
        for item in &report.items {
            println!(
                "{}: {} ({})",
                item.provider_locator,
                user_state_name(&item.user_state),
                next_action_name(&item.next_action)
            );
        }
    }
    if report.remaining > 0 {
        println!(
            "나머지 {}개는 다음 실행에서 확인할 수 있습니다.",
            report.remaining
        );
    }
    Ok(())
}

fn emit_status_human(report: &StatusReport) -> Result<(), MkoError> {
    println!(
        "새 항목 {} · 등록됨 {} · 미완료 {} · 검토 대기 {} · 완료 {} · 막힘 {}",
        report.counts[&UserState::New],
        report.counts[&UserState::Registered],
        report.counts[&UserState::Incomplete],
        report.counts[&UserState::ReviewPending],
        report.counts[&UserState::Processed],
        report.counts[&UserState::Blocked],
    );
    if report.next_action == NextAction::None {
        println!("지금 필요한 작업이 없습니다.");
    } else {
        println!("다음 작업: {}", next_action_name(&report.next_action));
    }
    Ok(())
}

fn user_state_name(state: &UserState) -> &'static str {
    match state {
        UserState::New => "새 항목",
        UserState::Registered => "등록됨",
        UserState::Incomplete => "미완료",
        UserState::ReviewPending => "검토 대기",
        UserState::Processed => "완료",
        UserState::Blocked => "막힘",
    }
}

fn next_action_name(action: &NextAction) -> &'static str {
    match action {
        NextAction::None => "없음",
        NextAction::Configure => "설정",
        NextAction::Hydrate => "파일 다운로드",
        NextAction::Add => "등록",
        NextAction::Prepare => "내용 추출",
        NextAction::WriteDraft => "초안 작성",
        NextAction::Review => "검토",
        NextAction::Repair => "복구",
        NextAction::Retry => "다시 시도",
    }
}

fn emit_doctor_human(report: &mko_core::doctor::DoctorReport) -> Result<(), MkoError> {
    if report.healthy {
        println!("설정과 연결 상태가 정상입니다.");
        return Ok(());
    }
    let message = match report.next_action {
        mko_core::json_v1::NextAction::Configure => {
            "설정이 필요합니다. 프로필과 저장소 설정을 확인한 뒤 다시 시도하세요."
        }
        mko_core::json_v1::NextAction::Hydrate => {
            "PDF를 아직 읽을 수 없습니다. 동기화가 끝난 뒤 파일을 한 번 열고 다시 시도하세요."
        }
        mko_core::json_v1::NextAction::Retry => {
            "다른 작업이 진행 중입니다. 잠시 후 다시 시도하세요."
        }
        _ => "설정을 복구해야 합니다. 진단 결과를 확인한 뒤 다시 시도하세요.",
    };
    println!("{message}");
    Ok(())
}

fn check_data(report: &CheckReport) -> CheckData {
    CheckData {
        valid: report.is_ok(),
        errors: report
            .issues
            .iter()
            .map(|issue| DiagnosticData {
                code: issue.code.clone(),
                message: issue.message.clone(),
                path: issue.path.clone(),
            })
            .collect(),
        warnings: Vec::new(),
    }
}

fn setup(arguments: SetupArgs) -> Result<(), MkoError> {
    if let Some(command) = arguments.command {
        return match command {
            SetupCommand::Plan(arguments) => setup_plan_v2(arguments),
            SetupCommand::Apply(arguments) => setup_apply_v2(arguments),
        };
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(MkoError::new(
            "tty_required",
            "setup requires an interactive terminal",
        ));
    }
    let platform = SystemPlatformEnvironment;
    let repository = setup_destination(arguments.repo, &platform)?;
    let drive_root = select_drive_root(arguments.drive_root, &platform)?;
    let marker_exists = repository.join("knowledge-os.yaml").exists();
    if marker_exists && !crate::cli_v2::is_v2_repository(&repository)? {
        return setup_legacy(repository, drive_root);
    }

    let inbox = drive_root.join("My-Knowledge-OS-Assets/personal/inbox");
    println!("Personal KB: {}", repository.display());
    println!("Personal Inbox: {}", inbox.display());
    print!("이 위치로 설정할까요? [y/N]: ");
    std::io::stdout()
        .flush()
        .map_err(|error| MkoError::new("terminal_write_failed", error.to_string()))?;
    let mut confirmation = String::new();
    std::io::stdin()
        .read_line(&mut confirmation)
        .map_err(|error| MkoError::new("terminal_read_failed", error.to_string()))?;
    if !matches!(
        confirmation.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ) {
        return Err(MkoError::new(
            "setup_cancelled",
            "setup was cancelled before mutation",
        ));
    }
    let result = setup_personal_v2(
        SetupPersonalV2Request {
            repository_root: &repository,
            drive_account_root: &drive_root,
            replace_profile: arguments.replace_profile,
        },
        &platform,
    )?;
    println!();
    println!("My Knowledge OS 준비 완료");
    println!("✓ Personal KB 연결: {}", result.repository_root.display());
    println!(
        "✓ Google Drive Inbox 연결: {}",
        result.provider_root.display()
    );
    println!("✓ Obsidian 보기 파일 생성");
    println!("PDF를 Inbox에 넣고 `mko add <PDF 경로>`를 실행하세요.");
    Ok(())
}

fn setup_plan_v2(arguments: SetupPlanArgs) -> Result<(), MkoError> {
    if arguments.format != OutputFormat::JsonV2 {
        return Err(MkoError::new(
            "format_unsupported",
            "mko setup plan requires json-v2 output",
        ));
    }
    let platform = SystemPlatformEnvironment;
    let repository = setup_destination(arguments.repo, &platform)?;
    let drive_root = select_drive_root_machine(arguments.drive_root, &platform)?;
    let plan = create_setup_plan_v2(
        SetupPersonalV2Request {
            repository_root: &repository,
            drive_account_root: &drive_root,
            replace_profile: arguments.replace_profile,
        },
        &platform,
        &SystemClock,
    )?;
    crate::output::emit_json_v2(JsonV2Success::setup_plan(plan))
}

fn setup_apply_v2(arguments: SetupApplyArgs) -> Result<(), MkoError> {
    if arguments.format != OutputFormat::JsonV2 {
        return Err(MkoError::new(
            "format_unsupported",
            "mko setup apply requires json-v2 output",
        ));
    }
    let result =
        apply_setup_plan_v2_tty(&arguments.plan, &SystemPlatformEnvironment, &SystemClock)?;
    crate::output::emit_json_v2(JsonV2Success::setup_apply(SetupApplyDataV2 {
        plan_id: result.plan_id,
        repository_root: result.setup.repository_root.display().to_string(),
        provider_root: result.setup.provider_root.display().to_string(),
        profile_changed: result.setup.profile_changed,
    }))
}

fn setup_legacy(repository: PathBuf, drive_root: PathBuf) -> Result<(), MkoError> {
    let preflight = preflight_setup(
        SetupRequest::new(repository).with_drive_root(drive_root),
        &SystemPlatformEnvironment,
    )?;
    let outcome = apply_setup(preflight, &SystemSetupWriter)?;
    if let Some(failure) = outcome.failure {
        return Err(MkoError::new(failure.code, failure.message));
    }
    println!("setup complete");
    Ok(())
}

fn setup_destination(
    explicit: Option<PathBuf>,
    platform: &dyn mko_core::context::PlatformEnvironment,
) -> Result<PathBuf, MkoError> {
    if let Some(repository) = explicit {
        return Ok(repository);
    }
    if let Ok(repository) = setup_repository(None) {
        return Ok(repository);
    }
    Ok(platform.home_dir()?.join("My-Knowledge-OS"))
}

fn select_drive_root(
    explicit: Option<PathBuf>,
    platform: &dyn mko_core::context::PlatformEnvironment,
) -> Result<PathBuf, MkoError> {
    if let Some(root) = explicit {
        return Ok(root);
    }
    let roots = detect_google_drive_roots(platform)?;
    if roots.len() == 1 {
        return Ok(roots[0].path.clone());
    }
    if roots.is_empty() {
        return Err(MkoError::new(
            "drive_root_not_found",
            "no platform-known Google Drive account root was found",
        ));
    }
    for (index, root) in roots.iter().enumerate() {
        println!("{}: {}", index + 1, root.path.display());
    }
    print!("Select Google Drive account: ");
    std::io::stdout()
        .flush()
        .map_err(|error| MkoError::new("terminal_write_failed", error.to_string()))?;
    let mut choice = String::new();
    std::io::stdin()
        .read_line(&mut choice)
        .map_err(|error| MkoError::new("terminal_read_failed", error.to_string()))?;
    choice
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|number| roots.get(number.saturating_sub(1)))
        .map(|selected| selected.path.clone())
        .ok_or_else(|| {
            MkoError::new(
                "drive_root_ambiguous",
                "select a listed Google Drive account",
            )
        })
}

fn select_drive_root_machine(
    explicit: Option<PathBuf>,
    platform: &dyn mko_core::context::PlatformEnvironment,
) -> Result<PathBuf, MkoError> {
    if let Some(root) = explicit {
        return Ok(root);
    }
    let roots = detect_google_drive_roots(platform)?;
    match roots.as_slice() {
        [root] => Ok(root.path.clone()),
        [] => Err(MkoError::new(
            "drive_root_not_found",
            "no platform-known Google Drive account root was found",
        )),
        _ => Err(MkoError::new(
            "drive_root_ambiguous",
            "pass --drive-root with one detected Google Drive account root",
        )),
    }
}

fn dashboard(arguments: DashboardArgs) -> Result<(), MkoError> {
    let repository = setup_repository(arguments.repo)?;
    if !crate::cli_v2::is_v2_repository(&repository)? {
        return Err(MkoError::new(
            "kb_schema_unsupported",
            "mko dashboard requires a v0.3 Personal KB",
        ));
    }
    if arguments.format == OutputFormat::JsonV1 {
        return Err(MkoError::new(
            "format_unsupported",
            "mko dashboard supports human or json-v2 output",
        ));
    }
    if arguments.repair {
        let result = repair_dashboard_v2(&repository)?;
        if arguments.format == OutputFormat::Human {
            match result.outcome {
                DashboardOutcomeV2::Created => println!("Obsidian 보기 파일을 생성했습니다."),
                DashboardOutcomeV2::Existing => println!("Obsidian 보기 파일이 정상입니다."),
                DashboardOutcomeV2::Repaired => {
                    println!("변경되지 않은 생성 파일을 안전하게 복구했습니다.")
                }
            }
            for path in result.generated_files {
                println!("- {path}");
            }
        }
    }
    let status = inspect_dashboard_v2(&repository)?;
    if arguments.format == OutputFormat::JsonV2 {
        let data = dashboard_json_v2(status);
        return crate::output::emit_json_v2(JsonV2Success::dashboard(data));
    }
    println!(
        "Canonical: {}",
        match status.canonical_state {
            DashboardCanonicalStateV2::Ready => "ready",
            DashboardCanonicalStateV2::Blocked => "blocked",
        }
    );
    println!(
        "Projection: {}",
        match status.projection_state {
            DashboardProjectionStateV2::Current => "current",
            DashboardProjectionStateV2::RepairRequired => "repair required",
        }
    );
    for item in &status.items {
        println!("- {}: {:?}", item.path, item.state);
    }
    Ok(())
}

fn dashboard_json_v2(status: mko_core::dashboard_v2::DashboardStatusV2) -> DashboardDataV2 {
    let preserve_user_edit = status.items.iter().any(|item| {
        matches!(
            item.state,
            DashboardFileStateV2::UserModified | DashboardFileStateV2::Orphaned
        )
    });
    DashboardDataV2 {
        canonical_state: match status.canonical_state {
            DashboardCanonicalStateV2::Ready => DashboardCanonicalStateDataV2::Ready,
            DashboardCanonicalStateV2::Blocked => DashboardCanonicalStateDataV2::Blocked,
        },
        projection_state: match status.projection_state {
            DashboardProjectionStateV2::Current => DashboardProjectionStateDataV2::Current,
            DashboardProjectionStateV2::RepairRequired => {
                DashboardProjectionStateDataV2::RepairRequired
            }
        },
        manifest_owned_drift: status.manifest_owned_drift,
        next_action: match (status.projection_state, preserve_user_edit) {
            (DashboardProjectionStateV2::Current, _) => NextActionV2::None,
            (DashboardProjectionStateV2::RepairRequired, true) => NextActionV2::PreserveUserEdit,
            (DashboardProjectionStateV2::RepairRequired, false) => NextActionV2::Repair,
        },
        items: status
            .items
            .into_iter()
            .map(|item| DashboardFileDataV2 {
                path: item.path,
                kind: match item.kind {
                    DashboardFileKindV2::ViewDefinition => DashboardFileKindDataV2::ViewDefinition,
                    DashboardFileKindV2::RecordProjection => {
                        DashboardFileKindDataV2::RecordProjection
                    }
                },
                manifest_owned: item.manifest_owned,
                next_action: match item.state {
                    DashboardFileStateV2::Current => NextActionV2::None,
                    DashboardFileStateV2::UserModified | DashboardFileStateV2::Orphaned => {
                        NextActionV2::PreserveUserEdit
                    }
                    DashboardFileStateV2::Missing
                    | DashboardFileStateV2::Stale
                    | DashboardFileStateV2::Unowned => NextActionV2::Repair,
                },
                state: match item.state {
                    DashboardFileStateV2::Current => DashboardFileStateDataV2::Current,
                    DashboardFileStateV2::Missing => DashboardFileStateDataV2::Missing,
                    DashboardFileStateV2::Stale => DashboardFileStateDataV2::Stale,
                    DashboardFileStateV2::UserModified => DashboardFileStateDataV2::UserModified,
                    DashboardFileStateV2::Unowned => DashboardFileStateDataV2::Unowned,
                    DashboardFileStateV2::Orphaned => DashboardFileStateDataV2::Orphaned,
                },
            })
            .collect(),
        scan_complete: true,
        remaining: 0,
        next_cursor: None,
    }
}

fn setup_repository(explicit: Option<PathBuf>) -> Result<PathBuf, MkoError> {
    if let Some(repository) = explicit {
        return Ok(repository);
    }
    let current = std::env::current_dir()
        .map_err(|error| MkoError::new("current_directory_unavailable", error.to_string()))?;
    for candidate in current.ancestors() {
        if candidate.join("knowledge-os.yaml").is_file() {
            return Ok(candidate.to_path_buf());
        }
    }
    resolve_context(None).map(|context| context.repository_root)
}

fn resolve_context(repository: Option<PathBuf>) -> Result<ResolvedPersonalContext, MkoError> {
    let request = match repository {
        Some(path) => ResolveContextRequest::new().with_explicit_repository(path),
        None => ResolveContextRequest::new(),
    };
    resolve_personal_context(request, &SystemPlatformEnvironment)
}

fn normalized_runtime_output(
    repository: &Path,
    asset_id: &str,
    output: &Path,
) -> Result<PathBuf, MkoError> {
    let expected = PathBuf::from(".knowledge-os")
        .join("runtime")
        .join("prepared")
        .join(format!("{asset_id}.json"));
    if output.is_absolute() {
        let mut selected_repository = output.to_path_buf();
        for component in expected.components().rev() {
            if selected_repository.file_name() != Some(component.as_os_str())
                || !selected_repository.pop()
            {
                return Err(runtime_output_error());
            }
        }
        let selected_repository =
            std::fs::canonicalize(selected_repository).map_err(|_| runtime_output_error())?;
        if selected_repository != repository {
            return Err(runtime_output_error());
        }
    } else if output != expected {
        return Err(runtime_output_error());
    }
    Ok(repository.join(expected))
}

fn runtime_output_error() -> MkoError {
    MkoError::new(
        "runtime_output_invalid",
        "output must be .knowledge-os/runtime/prepared/<asset-id>.json beneath the selected repository",
    )
}

fn format_is_json_v1(format: Option<OutputFormat>) -> bool {
    format == Some(OutputFormat::JsonV1)
}

fn repair_source(arguments: RepairStateArgs) -> Result<(), MkoError> {
    let result = repair_source_state(
        RepairSourceStateRequest::new(&arguments.repo, &arguments.asset_id)
            .with_clear_stale_lock(arguments.clear_stale_lock),
    )?;
    if arguments.json {
        emit_json_value(
            &serde_json::json!({"result":result.result,"source_id":result.source_id,"asset_id":result.asset_id}),
        )?;
    } else {
        println!("{} {} {}", result.result, result.source_id, result.asset_id);
    }
    Ok(())
}
fn extract_pdf() -> Result<(), MkoError> {
    let response = match extract_pdf_pages_from_reader(std::io::stdin().lock()) {
        Ok(pages) => ExtractionWorkerResponse::Success { pages },
        Err(error) => ExtractionWorkerResponse::Error {
            code: error.code().into(),
            message: error.message().into(),
        },
    };
    emit_encoded_json(
        &serde_json::to_string(&response)
            .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?,
    )
}
fn capture(arguments: CaptureArgs) -> Result<(), MkoError> {
    let mut request = CaptureRequest::new(arguments.repo, arguments.file)
        .with_title(arguments.title)
        .with_domains(arguments.domains)
        .with_slug(arguments.slug);
    if let Some(config) = arguments.local_config {
        request = request.with_local_config(config);
    }
    let result = capture_asset(request)?;
    if arguments.json {
        emit_json_value(
            &serde_json::json!({"result":result.result,"asset_id":result.asset_id,"registry_path":result.registry_path}),
        )?;
    } else {
        println!(
            "{} {} {}",
            result.result, result.asset_id, result.registry_path
        );
    }
    Ok(())
}
fn inspect(arguments: AssetOperationArgs) -> Result<(), MkoError> {
    let json = arguments.json;
    let asset = inspect_asset(operation_request(&arguments))?;
    let status = asset_status_name(&asset.asset_status);
    if json {
        emit_json_value(&serde_json::json!({"result":status,"asset_id":asset.id}))?;
    } else {
        println!("{status} {}", asset.id);
    }
    Ok(())
}
fn accept_change(arguments: AssetOperationArgs) -> Result<(), MkoError> {
    let json = arguments.json;
    let asset = accept_changed_asset(operation_request(&arguments))?;
    if json {
        emit_json_value(
            &serde_json::json!({"result":"accepted","asset_id":asset.id,"supersedes":asset.supersedes}),
        )?;
    } else {
        println!(
            "accepted {} supersedes {}",
            asset.id,
            asset.supersedes.unwrap_or_default()
        );
    }
    Ok(())
}
fn repair_asset_lineage(arguments: AssetOperationArgs) -> Result<(), MkoError> {
    let json = arguments.json;
    let asset_id = arguments.asset_id.clone();
    repair_lineage(operation_request(&arguments))?;
    if json {
        emit_json_value(&serde_json::json!({"result":"repaired","asset_id":asset_id}))?;
    } else {
        println!("repaired {asset_id}");
    }
    Ok(())
}
fn approve(arguments: ApproveSourceArgs) -> Result<(), MkoError> {
    let result = approve_source(ApproveSourceRequest::new(
        &arguments.repo,
        &arguments.source_id,
    ))?;
    if arguments.json {
        emit_json_value(
            &serde_json::json!({"result":"approved","source_id":result.source_id,"source_path":result.source_path,"revision":result.revision}),
        )?;
    } else {
        println!("approved {} {}", result.source_id, result.revision);
    }
    Ok(())
}
fn install_hook(arguments: HookInstallArgs) -> Result<(), MkoError> {
    let result = install_hooks(&arguments.repo)?;
    if arguments.json {
        emit_json_value(&serde_json::json!({"result":result.result,"hook_path":result.hook_path}))?;
    } else {
        println!("{} {}", result.result, result.hook_path);
    }
    Ok(())
}
fn operation_request(arguments: &AssetOperationArgs) -> AssetOperationRequest {
    let mut request = AssetOperationRequest::new(&arguments.repo, &arguments.asset_id)
        .with_clear_stale_lock(arguments.clear_stale_lock);
    if let Some(config) = &arguments.local_config {
        request = request.with_local_config(config);
    }
    request
}
fn asset_status_name(status: &AssetStatus) -> &'static str {
    match status {
        AssetStatus::Registered => "registered",
        AssetStatus::Extracted => "extracted",
        AssetStatus::ReviewPending => "review_pending",
        AssetStatus::Processed => "processed",
        AssetStatus::Changed => "changed",
        AssetStatus::Missing => "missing",
        AssetStatus::Superseded => "superseded",
        AssetStatus::Failed => "failed",
    }
}
fn draft_outcome(result: &str) -> Result<DraftOutcome, MkoError> {
    match result {
        "created" => Ok(DraftOutcome::Created),
        "existing" => Ok(DraftOutcome::Existing),
        "replaced" => Ok(DraftOutcome::Replaced),
        _ => Err(MkoError::new(
            "draft_result_invalid",
            "write draft returned an unknown result",
        )),
    }
}
fn emit_legacy_error(code: &str, message: &str, json: bool) {
    if json {
        if let Err(output_error) = emit_legacy_json_error(code, message) {
            eprintln!("{}: {}", output_error.code(), output_error.message());
        }
    } else {
        eprintln!("{code}: {message}");
    }
}
fn emit_json_v1_failure_or_stderr(command: JsonV1Command, error: &MkoError) {
    if let Err(output_error) = emit_json_v1_failure(command, error) {
        eprintln!("{}: {}", output_error.code(), output_error.message());
    }
}

fn emit_json_v2_failure_or_stderr(command: JsonV2Command, error: &MkoError) {
    if let Err(output_error) = emit_json_v2_failure(command, error) {
        eprintln!("{}: {}", output_error.code(), output_error.message());
    }
}

fn json_v2_command(cli: &Cli) -> Option<JsonV2Command> {
    match cli.command.as_ref()? {
        Command::Handshake(HandshakeArgs {
            format: OutputFormat::JsonV2,
            ..
        }) => Some(JsonV2Command::Handshake),
        Command::Doctor(DoctorArgs {
            format: OutputFormat::JsonV2,
            ..
        }) => Some(JsonV2Command::Doctor),
        Command::Schema {
            command:
                SchemaCommand::List(SchemaListArgs {
                    format: OutputFormat::JsonV2,
                }),
        } => Some(JsonV2Command::SchemaList),
        Command::Schema {
            command:
                SchemaCommand::Show(SchemaShowArgs {
                    format: OutputFormat::JsonV2,
                    ..
                }),
        } => Some(JsonV2Command::SchemaShow),
        Command::Setup(SetupArgs {
            command:
                Some(SetupCommand::Plan(SetupPlanArgs {
                    format: OutputFormat::JsonV2,
                    ..
                })),
            ..
        }) => Some(JsonV2Command::SetupPlan),
        Command::Setup(SetupArgs {
            command:
                Some(SetupCommand::Apply(SetupApplyArgs {
                    format: OutputFormat::JsonV2,
                    ..
                })),
            ..
        }) => Some(JsonV2Command::SetupApply),
        Command::Add(AddArgs {
            format: OutputFormat::JsonV2,
            ..
        }) => Some(JsonV2Command::Add),
        Command::Queue(QueueArgs {
            format: OutputFormat::JsonV2,
            ..
        }) => Some(JsonV2Command::Queue),
        Command::Show(ShowArgs {
            format: OutputFormat::JsonV2,
            ..
        }) => Some(JsonV2Command::Show),
        Command::ReviewOpen(ReviewOpenArgs {
            format: OutputFormat::JsonV2,
            ..
        }) => Some(JsonV2Command::ReviewOpen),
        Command::ReviewFeedback(ReviewFeedbackArgs {
            format: OutputFormat::JsonV2,
            ..
        }) => Some(JsonV2Command::ReviewFeedback),
        Command::Dashboard(DashboardArgs {
            format: OutputFormat::JsonV2,
            ..
        }) => Some(JsonV2Command::Dashboard),
        Command::Source {
            command:
                SourceCommand::Prepare(PrepareArgs {
                    format: Some(OutputFormat::JsonV2),
                    ..
                }),
        } => Some(JsonV2Command::SourcePrepare),
        Command::Source {
            command:
                SourceCommand::WriteDraft(WriteDraftArgs {
                    format: Some(OutputFormat::JsonV2),
                    ..
                }),
        } => Some(JsonV2Command::SourceWrite),
        Command::Knowledge {
            command:
                KnowledgeCommand::Write(KnowledgeWriteArgs {
                    format: Some(OutputFormat::JsonV2),
                    ..
                }),
        } => Some(JsonV2Command::KnowledgeWrite),
        _ => None,
    }
}

fn json_v1_command(cli: &Cli) -> Option<JsonV1Command> {
    match cli.command.as_ref()? {
        Command::Add(AddArgs {
            format: OutputFormat::JsonV1,
            ..
        }) => Some(JsonV1Command::Add),
        Command::Check(CheckArgs {
            format: Some(OutputFormat::JsonV1),
            ..
        }) => Some(JsonV1Command::Check),
        Command::Doctor(DoctorArgs {
            format: OutputFormat::JsonV1,
            ..
        }) => Some(JsonV1Command::Doctor),
        Command::Inbox(InboxArgs {
            format: OutputFormat::JsonV1,
            ..
        }) => Some(JsonV1Command::Inbox),
        Command::Status(StatusArgs {
            format: OutputFormat::JsonV1,
            ..
        }) => Some(JsonV1Command::Status),
        Command::Source {
            command:
                SourceCommand::Prepare(PrepareArgs {
                    format: Some(OutputFormat::JsonV1),
                    ..
                }),
        } => Some(JsonV1Command::SourcePrepare),
        Command::Source {
            command:
                SourceCommand::WriteDraft(WriteDraftArgs {
                    format: Some(OutputFormat::JsonV1),
                    ..
                }),
        } => Some(JsonV1Command::SourceWriteDraft),
        Command::Knowledge {
            command:
                KnowledgeCommand::Write(KnowledgeWriteArgs {
                    format: Some(OutputFormat::JsonV1),
                    ..
                }),
        } => Some(JsonV1Command::KnowledgeWrite),
        Command::Knowledge {
            command:
                KnowledgeCommand::Review(KnowledgeReviewArgs {
                    format: Some(OutputFormat::JsonV1),
                    ..
                }),
        } => Some(JsonV1Command::KnowledgeReview),
        Command::Knowledge {
            command:
                KnowledgeCommand::Search(KnowledgeSearchArgs {
                    format: Some(OutputFormat::JsonV1),
                    ..
                }),
        } => Some(JsonV1Command::KnowledgeSearch),
        Command::Knowledge {
            command:
                KnowledgeCommand::Show(KnowledgeShowArgs {
                    format: Some(OutputFormat::JsonV1),
                    ..
                }),
        } => Some(JsonV1Command::KnowledgeShow),
        Command::Knowledge {
            command:
                KnowledgeCommand::List(KnowledgeListArgs {
                    format: Some(OutputFormat::JsonV1),
                    ..
                }),
        } => Some(JsonV1Command::KnowledgeList),
        _ => None,
    }
}

fn json_v1_command_from_invalid_arguments(args: &[std::ffi::OsString]) -> Option<JsonV1Command> {
    let args = arguments_before_terminator(args);
    let json_v1 = args
        .windows(2)
        .any(|pair| pair[0] == "--format" && pair[1] == "json-v1")
        || args.iter().any(|argument| argument == "--format=json-v1");
    if !json_v1 {
        return None;
    }
    match (
        args.get(1)?.to_str()?,
        args.get(2).and_then(|argument| argument.to_str()),
    ) {
        ("add", _) => Some(JsonV1Command::Add),
        ("check", _) => Some(JsonV1Command::Check),
        ("doctor", _) => Some(JsonV1Command::Doctor),
        ("inbox", _) => Some(JsonV1Command::Inbox),
        ("status", _) => Some(JsonV1Command::Status),
        ("source", Some("prepare")) => Some(JsonV1Command::SourcePrepare),
        ("source", Some("write-draft")) => Some(JsonV1Command::SourceWriteDraft),
        ("knowledge", Some("write")) => Some(JsonV1Command::KnowledgeWrite),
        ("knowledge", Some("review")) => Some(JsonV1Command::KnowledgeReview),
        ("knowledge", Some("search")) => Some(JsonV1Command::KnowledgeSearch),
        ("knowledge", Some("show")) => Some(JsonV1Command::KnowledgeShow),
        ("knowledge", Some("list")) => Some(JsonV1Command::KnowledgeList),
        _ => None,
    }
}

fn json_v2_command_from_invalid_arguments(args: &[std::ffi::OsString]) -> Option<JsonV2Command> {
    let args = arguments_before_terminator(args);
    let json_v2 = args
        .windows(2)
        .any(|pair| pair[0] == "--format" && pair[1] == "json-v2")
        || args.iter().any(|argument| argument == "--format=json-v2");
    if !json_v2 {
        return None;
    }
    match (
        args.get(1)?.to_str()?,
        args.get(2).and_then(|argument| argument.to_str()),
    ) {
        ("handshake", _) => Some(JsonV2Command::Handshake),
        ("doctor", _) => Some(JsonV2Command::Doctor),
        ("schema", Some("list")) => Some(JsonV2Command::SchemaList),
        ("schema", Some("show")) => Some(JsonV2Command::SchemaShow),
        ("setup", Some("plan")) => Some(JsonV2Command::SetupPlan),
        ("setup", Some("apply")) => Some(JsonV2Command::SetupApply),
        ("add", _) => Some(JsonV2Command::Add),
        ("queue", _) => Some(JsonV2Command::Queue),
        ("show", _) => Some(JsonV2Command::Show),
        ("review-open", _) => Some(JsonV2Command::ReviewOpen),
        ("review-feedback", _) => Some(JsonV2Command::ReviewFeedback),
        ("dashboard", _) => Some(JsonV2Command::Dashboard),
        ("source", Some("prepare")) => Some(JsonV2Command::SourcePrepare),
        ("source", Some("write-draft")) => Some(JsonV2Command::SourceWrite),
        ("knowledge", Some("write")) => Some(JsonV2Command::KnowledgeWrite),
        _ => None,
    }
}

fn legacy_json_requested_from_invalid_arguments(args: &[std::ffi::OsString]) -> bool {
    if !args.iter().any(|argument| argument == "--json") {
        return false;
    }
    // Frozen v0.1 behavior: any argument equal to `--json` selects legacy JSON error output for the
    // legacy command families (asset/source/check/human/hooks/__extract-pdf) and for invocations
    // that name no recognized subcommand at all. The v0.2-only commands below never accepted
    // `--json`, so a parse failure there is an ordinary usage error, not legacy JSON output.
    !matches!(
        args.get(1).and_then(|argument| argument.to_str()),
        Some(
            "setup"
                | "add"
                | "find"
                | "remember"
                | "doctor"
                | "inbox"
                | "status"
                | "review"
                | "knowledge"
        )
    )
}

fn arguments_before_terminator(args: &[std::ffi::OsString]) -> &[std::ffi::OsString] {
    let end = args
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(args.len());
    &args[..end]
}

fn legacy_usage_error(args: &[std::ffi::OsString]) -> Option<MkoError> {
    if !legacy_json_requested_from_invalid_arguments(args) {
        return None;
    }
    let mut normalized = args.to_vec();
    if let Some(program) = normalized.first_mut() {
        *program = "mko".into();
    }
    LegacyCli::try_parse_from(normalized)
        .err()
        .map(|error| MkoError::new("usage", error.to_string()))
}

trait AddOutcomeName {
    fn add_outcome_string(&self) -> &'static str;
}
impl AddOutcomeName for mko_core::add::AddResult {
    fn add_outcome_string(&self) -> &'static str {
        match self.add_outcome {
            mko_core::json_v1::AddOutcome::Created => "created",
            mko_core::json_v1::AddOutcome::Existing => "existing",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_perspective_numbers_are_many_to_many_and_strict() {
        assert_eq!(
            parse_perspective_numbers("5, 3,3").unwrap(),
            vec![PerspectiveV2::Technical, PerspectiveV2::Investment]
        );
        assert_eq!(
            parse_perspective_numbers("0").unwrap_err().code(),
            "perspective_selection_invalid"
        );
        assert_eq!(
            parse_perspective_numbers("later").unwrap_err().code(),
            "perspective_selection_invalid"
        );
    }
}
