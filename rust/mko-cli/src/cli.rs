use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use mko_core::{
    add::{AddInput, AddRequest, AddRunResult, BackupAttestation, add as add_input},
    approve::{ApproveSourceRequest, approve_source},
    check::{CheckReport, CheckRequest, check_repository},
    clock::SystemClock,
    context::{
        ResolveContextRequest, ResolvedPersonalContext, SystemPlatformEnvironment,
        resolve_personal_context,
    },
    doctor::{DoctorRequest, SystemDoctorEnvironment, diagnose},
    error::MkoError,
    hooks::install_hooks,
    inbox::{InboxScanRequest, InboxScanResult, scan_inbox},
    json_v1::{
        AddData, AddPayload, CheckData, ConceptMatchData, DiagnosticData, DoctorCheckData,
        DoctorData, DraftOutcome, JsonV1Command, JsonV1Success, KnowledgeConceptSummary,
        KnowledgeListData, KnowledgePendingItemData, KnowledgeReviewData, KnowledgeReviewDecision,
        KnowledgeReviewItemData, KnowledgeReviewStatusData, KnowledgeSearchData, KnowledgeShowData,
        KnowledgeWriteData, KnowledgeWriteOutcome, NextAction, PrepareData, Recovery,
        SuccessResult, UserState, WriteDraftData,
    },
    knowledge::{
        ConceptKind, ConceptMatch, KnowledgeSearchQuery, WriteKnowledgeRequest, approve_knowledge,
        list_unreviewed_knowledge, search_knowledge, write_knowledge_note,
    },
    model::AssetStatus,
    pdf::{ExtractionWorkerResponse, extract_pdf_pages_from_reader, worker_executable},
    prepare::{PrepareRequest, prepare_source},
    provider_scan::MonotonicElapsedClock,
    registry::{
        AssetOperationRequest, CaptureRequest, accept_changed_asset, capture_asset, inspect_asset,
        repair_lineage,
    },
    review::{ReviewOutcome, review as review_pending},
    setup::{
        SetupRequest, SystemSetupWriter, apply_setup, detect_google_drive_roots, preflight_setup,
    },
    source::{
        RepairSourceStateRequest, WriteSourceRequest, repair_source_state, write_source_draft,
    },
    status::{StatusReport, status_from_inbox},
};

use crate::{
    batch_add_data,
    output::{
        emit_encoded_json, emit_json_v1, emit_json_v1_failure, emit_json_value,
        emit_legacy_json_error,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Human,
    JsonV1,
}

#[derive(Parser)]
#[command(name = "mko", version = mko_core::version::PRODUCT_VERSION, about = "My Knowledge OS deterministic core")]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
    Setup(SetupArgs),
    Add(AddArgs),
    Asset {
        #[command(subcommand)]
        command: AssetCommand,
    },
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    Check(CheckArgs),
    Doctor(DoctorArgs),
    Inbox(InboxArgs),
    Status(StatusArgs),
    Review(ReviewArgs),
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommand,
    },
    Human {
        #[command(subcommand)]
        command: HumanCommand,
    },
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    #[command(name = "__extract-pdf", hide = true)]
    ExtractPdf,
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
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long)]
    drive_root: Option<PathBuf>,
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
struct ReviewArgs {
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
    output: PathBuf,
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
            let legacy_check_requested = matches!(
                &cli.command,
                Command::Check(CheckArgs {
                    format: None | Some(OutputFormat::Human),
                    ..
                })
            );
            match run(cli) {
                Ok(Exit::Success) => {}
                Ok(Exit::ValidationFailed) => std::process::exit(1),
                Err(error) => {
                    if let Some(command) = json_v1_command {
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
            if let Some(command) = json_v1_command_from_invalid_arguments(&args) {
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

fn run(cli: Cli) -> Result<Exit, MkoError> {
    match cli.command {
        Command::Setup(arguments) => setup(arguments).map(|_| Exit::Success),
        Command::Add(arguments) => add(arguments).map(|_| Exit::Success),
        Command::Asset {
            command: AssetCommand::Capture(arguments),
        } => capture(arguments).map(|_| Exit::Success),
        Command::Asset {
            command: AssetCommand::Inspect(arguments),
        } => inspect(arguments).map(|_| Exit::Success),
        Command::Asset {
            command: AssetCommand::AcceptChange(arguments),
        } => accept_change(arguments).map(|_| Exit::Success),
        Command::Asset {
            command: AssetCommand::RepairLineage(arguments),
        } => repair_asset_lineage(arguments).map(|_| Exit::Success),
        Command::Check(arguments) => check(arguments),
        Command::Doctor(arguments) => doctor(arguments).map(|_| Exit::Success),
        Command::Inbox(arguments) => inbox(arguments).map(|_| Exit::Success),
        Command::Status(arguments) => status(arguments).map(|_| Exit::Success),
        Command::Review(arguments) => review(arguments).map(|_| Exit::Success),
        Command::Knowledge {
            command: KnowledgeCommand::Write(arguments),
        } => knowledge_write(arguments).map(|_| Exit::Success),
        Command::Knowledge {
            command: KnowledgeCommand::Review(arguments),
        } => knowledge_review(arguments).map(|_| Exit::Success),
        Command::Knowledge {
            command: KnowledgeCommand::Search(arguments),
        } => knowledge_search(arguments).map(|_| Exit::Success),
        Command::Knowledge {
            command: KnowledgeCommand::Show(arguments),
        } => knowledge_show(arguments).map(|_| Exit::Success),
        Command::Knowledge {
            command: KnowledgeCommand::List(arguments),
        } => knowledge_list(arguments).map(|_| Exit::Success),
        Command::Source {
            command: SourceCommand::Prepare(arguments),
        } => prepare(arguments).map(|_| Exit::Success),
        Command::Source {
            command: SourceCommand::WriteDraft(arguments),
        } => write_draft(arguments).map(|_| Exit::Success),
        Command::Source {
            command: SourceCommand::RepairState(arguments),
        } => repair_source(arguments).map(|_| Exit::Success),
        Command::ExtractPdf => extract_pdf().map(|_| Exit::Success),
        Command::Human {
            command: HumanCommand::ApproveSource(arguments),
        } => approve(arguments).map(|_| Exit::Success),
        Command::Hooks {
            command: HooksCommand::Install(arguments),
        } => install_hook(arguments).map(|_| Exit::Success),
    }
}

fn add(arguments: AddArgs) -> Result<(), MkoError> {
    let context = resolve_context(None).map_err(|error| {
        if arguments.inbox {
            map_add_inbox_error(error)
        } else {
            error
        }
    })?;
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

fn prepare(arguments: PrepareArgs) -> Result<(), MkoError> {
    if !format_is_json_v1(arguments.format) {
        return prepare_legacy(arguments);
    }
    let context = resolve_context(arguments.repo)?;
    let runtime_output = normalized_runtime_output(
        &context.repository_root,
        &arguments.asset_id,
        &arguments.output,
    )?;
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
    let mut request = PrepareRequest::new(
        arguments.repo.as_ref().unwrap(),
        &arguments.asset_id,
        &arguments.output,
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

fn doctor(arguments: DoctorArgs) -> Result<(), MkoError> {
    let request = match arguments.repo {
        Some(repository) => DoctorRequest::new().with_repository(repository),
        None => DoctorRequest::new(),
    };
    let environment = SystemDoctorEnvironment::default();
    let report = diagnose(request, &environment);
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
    let repository = match arguments.repo {
        Some(repository) => repository,
        None => resolve_context(None)?.repository_root,
    };
    match review_pending(&repository)? {
        ReviewOutcome::Deferred => println!("deferred"),
        ReviewOutcome::Approved(result) => {
            println!("approved {} {}", result.source_id, result.revision)
        }
    }
    Ok(())
}

fn knowledge_write(arguments: KnowledgeWriteArgs) -> Result<(), MkoError> {
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
    let pending = list_unreviewed_knowledge(&repository)?;
    let unreviewed = pending
        .into_iter()
        .find(|item| item.asset_id == arguments.asset_id);
    let all_matches = search_knowledge(
        &repository,
        &KnowledgeSearchQuery {
            term: String::new(),
            kind: None,
            tag: None,
        },
    )?;
    let concept_matches = all_matches
        .into_iter()
        .filter(|item| item.asset_id == arguments.asset_id)
        .collect::<Vec<_>>();
    if unreviewed.is_none() && concept_matches.is_empty() {
        return Err(MkoError::new(
            "knowledge_not_found",
            "no knowledge note was found for that asset",
        ));
    }
    let title = unreviewed
        .as_ref()
        .map(|item| item.title.clone())
        .or_else(|| concept_matches.first().map(|item| item.title.clone()))
        .unwrap_or_default();
    let knowledge_path = unreviewed
        .as_ref()
        .map(|item| item.knowledge_path.clone())
        .or_else(|| {
            concept_matches
                .first()
                .map(|item| item.knowledge_path.clone())
        })
        .unwrap_or_default();
    let review_status = if unreviewed.is_some() {
        KnowledgeReviewStatusData::Unreviewed
    } else {
        KnowledgeReviewStatusData::Reviewed
    };
    if json_v1 {
        emit_json_v1(JsonV1Success::KnowledgeShow {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: KnowledgeShowData {
                asset_id: arguments.asset_id,
                title,
                knowledge_path,
                review_status,
                knowledge_id: unreviewed.as_ref().map(|item| item.knowledge_id.clone()),
                content_revision: unreviewed
                    .as_ref()
                    .map(|item| item.content_revision.clone()),
                concepts: concept_matches
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
        println!("{} {} {}", arguments.asset_id, title, knowledge_path);
        for concept in &concept_matches {
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
    let pending = list_unreviewed_knowledge(&repository)?;
    if json_v1 {
        emit_json_v1(JsonV1Success::KnowledgeList {
            schema_version: 1,
            result: SuccessResult::Ok,
            data: KnowledgeListData {
                items: pending
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
        for item in &pending {
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
        let text = std::fs::read_to_string(repository.join(&note.knowledge_path))
            .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
        println!("{text}");
        print!("{} · {} — approve/defer: ", note.title, note.asset_id);
        std::io::stdout()
            .flush()
            .map_err(|error| MkoError::new("terminal_write_failed", error.to_string()))?;
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
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(MkoError::new(
            "tty_required",
            "setup requires an interactive terminal",
        ));
    }
    let repository = setup_repository(arguments.repo)?;
    let platform = SystemPlatformEnvironment;
    let drive_root = match arguments.drive_root {
        Some(root) => root,
        None => {
            let roots = detect_google_drive_roots(&platform)?;
            if roots.len() == 1 {
                roots[0].path.clone()
            } else if roots.is_empty() {
                return Err(MkoError::new(
                    "drive_root_not_found",
                    "no platform-known Google Drive account root was found",
                ));
            } else {
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
                let selected = choice
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|number| roots.get(number.saturating_sub(1)))
                    .ok_or_else(|| {
                        MkoError::new(
                            "drive_root_ambiguous",
                            "select a listed Google Drive account",
                        )
                    })?;
                selected.path.clone()
            }
        }
    };
    let preflight = preflight_setup(
        SetupRequest::new(repository).with_drive_root(drive_root),
        &platform,
    )?;
    let outcome = apply_setup(preflight, &SystemSetupWriter)?;
    if let Some(failure) = outcome.failure {
        return Err(MkoError::new(failure.code, failure.message));
    }
    println!("setup complete");
    Ok(())
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

fn json_v1_command(cli: &Cli) -> Option<JsonV1Command> {
    match &cli.command {
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
        Some("setup" | "add" | "doctor" | "inbox" | "status" | "review" | "knowledge")
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
    LegacyCli::try_parse_from(args)
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
