use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use mko_core::{
    add::{AddRequest, BackupAttestation, add_pdf},
    approve::{ApproveSourceRequest, approve_source},
    check::{CheckReport, CheckRequest, check_repository},
    clock::SystemClock,
    context::{
        ResolveContextRequest, ResolvedPersonalContext, SystemPlatformEnvironment,
        resolve_personal_context,
    },
    error::MkoError,
    hooks::install_hooks,
    json_v1::{
        AddData, AddPayload, CheckData, DiagnosticData, DraftOutcome, JsonV1Command, JsonV1Success,
        PrepareData, SuccessResult, WriteDraftData,
    },
    model::AssetStatus,
    pdf::{ExtractionWorkerResponse, extract_pdf_pages_from_reader, worker_executable},
    prepare::{PrepareRequest, prepare_source},
    provider_scan::MonotonicElapsedClock,
    registry::{
        AssetOperationRequest, CaptureRequest, accept_changed_asset, capture_asset, inspect_asset,
        repair_lineage,
    },
    setup::{
        SetupRequest, SystemSetupWriter, apply_setup, detect_google_drive_roots, preflight_setup,
    },
    source::{
        RepairSourceStateRequest, WriteSourceRequest, repair_source_state, write_source_draft,
    },
};

use crate::output::{emit_json_v1, emit_json_v1_failure};

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
    file: PathBuf,
    #[arg(long)]
    verified_backup: bool,
    #[arg(long)]
    temporary_source: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
    #[arg(long, conflicts_with = "format")]
    json: bool,
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
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long, conflicts_with = "format")]
    json: bool,
    #[arg(long)]
    staged: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
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
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long)]
    local_config: Option<PathBuf>,
    #[arg(long)]
    asset_id: String,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    clear_stale_lock: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}
#[derive(Args)]
struct WriteDraftArgs {
    #[arg(long)]
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
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
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
            let legacy_check_requested = matches!(
                &cli.command,
                Command::Check(CheckArgs {
                    format: OutputFormat::Human,
                    ..
                })
            );
            match run(cli) {
                Ok(Exit::Success) => {}
                Ok(Exit::ValidationFailed) => std::process::exit(1),
                Err(error) => {
                    if let Some(command) = json_v1_command(&args) {
                        let _ = emit_json_v1_failure(command, &error);
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
            let usage = MkoError::new("usage", error.to_string());
            if let Some(command) = json_v1_command(&args) {
                let _ = emit_json_v1_failure(command, &usage);
            } else {
                emit_legacy_error(
                    usage.code(),
                    usage.message(),
                    args.iter().any(|arg| arg == "--json"),
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
    let context = resolve_context(None)?;
    let request = AddRequest::new(context, arguments.file)
        .with_temporary_source(arguments.temporary_source)
        .with_backup_attestation(if arguments.verified_backup {
            BackupAttestation::UserVerified
        } else {
            BackupAttestation::OutsideOriginalRetained
        });
    let result = add_pdf(request, &SystemClock, &MonotonicElapsedClock::start())?;
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

fn prepare(arguments: PrepareArgs) -> Result<(), MkoError> {
    if arguments.format == OutputFormat::Human && arguments.repo.is_some() {
        return prepare_legacy(arguments);
    }
    let context = resolve_context(arguments.repo)?;
    let runtime_output = normalized_runtime_output(&context.repository_root, &arguments.output)?;
    let request = PrepareRequest::new(
        &context.repository_root,
        &arguments.asset_id,
        &runtime_output,
    )
    .with_resolved_context(context)
    .with_clear_stale_lock(arguments.clear_stale_lock);
    let bundle = prepare_source(request, &worker_executable()?)?;
    if arguments.format == OutputFormat::JsonV1 {
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
    let json_v1 = arguments.format == OutputFormat::JsonV1;
    let repository = if json_v1 || arguments.repo.is_none() {
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
        println!(
            "{}",
            serde_json::json!({"result":result.result,"source_id":result.source_id,"source_path":result.source_path,"content_revision":result.content_revision})
        );
        Ok(())
    } else {
        println!(
            "{} {} {} {}",
            result.result, result.source_id, result.source_path, result.content_revision
        );
        Ok(())
    }
}

fn check(arguments: CheckArgs) -> Result<Exit, MkoError> {
    let json_v1 = arguments.format == OutputFormat::JsonV1;
    let repository = if json_v1 || arguments.repo.is_none() {
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
        println!(
            "{}",
            serde_json::to_string(&report)
                .map_err(|error| MkoError::new("check_failed", error.to_string()))?
        );
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

fn normalized_runtime_output(repository: &Path, output: &Path) -> Result<PathBuf, MkoError> {
    let relative = if output.is_absolute() {
        output
            .components()
            .collect::<Vec<_>>()
            .windows(4)
            .find(|components| {
                components[0].as_os_str() == ".knowledge-os"
                    && components[1].as_os_str() == "runtime"
                    && components[2].as_os_str() == "prepared"
            })
            .map(|components| components.iter().collect::<PathBuf>())
            .ok_or_else(|| {
                MkoError::new(
                    "runtime_output_invalid",
                    "output must be beneath .knowledge-os/runtime/prepared",
                )
            })?
    } else {
        output.to_path_buf()
    };
    Ok(repository.join(relative))
}

fn repair_source(arguments: RepairStateArgs) -> Result<(), MkoError> {
    let result = repair_source_state(
        RepairSourceStateRequest::new(&arguments.repo, &arguments.asset_id)
            .with_clear_stale_lock(arguments.clear_stale_lock),
    )?;
    if arguments.json {
        println!(
            "{}",
            serde_json::json!({"result":result.result,"source_id":result.source_id,"asset_id":result.asset_id})
        );
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
    println!(
        "{}",
        serde_json::to_string(&response)
            .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?
    );
    Ok(())
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
        println!(
            "{}",
            serde_json::json!({"result":result.result,"asset_id":result.asset_id,"registry_path":result.registry_path})
        );
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
        println!(
            "{}",
            serde_json::json!({"result":status,"asset_id":asset.id})
        );
    } else {
        println!("{status} {}", asset.id);
    }
    Ok(())
}
fn accept_change(arguments: AssetOperationArgs) -> Result<(), MkoError> {
    let json = arguments.json;
    let asset = accept_changed_asset(operation_request(&arguments))?;
    if json {
        println!(
            "{}",
            serde_json::json!({"result":"accepted","asset_id":asset.id,"supersedes":asset.supersedes})
        );
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
        println!(
            "{}",
            serde_json::json!({"result":"repaired","asset_id":asset_id})
        );
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
        println!(
            "{}",
            serde_json::json!({"result":"approved","source_id":result.source_id,"source_path":result.source_path,"revision":result.revision})
        );
    } else {
        println!("approved {} {}", result.source_id, result.revision);
    }
    Ok(())
}
fn install_hook(arguments: HookInstallArgs) -> Result<(), MkoError> {
    let result = install_hooks(&arguments.repo)?;
    if arguments.json {
        println!(
            "{}",
            serde_json::json!({"result":result.result,"hook_path":result.hook_path})
        );
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
        println!(
            "{}",
            serde_json::json!({"result":"error","error":{"code":code,"message":message}})
        );
    } else {
        eprintln!("{code}: {message}");
    }
}
fn json_v1_command(args: &[std::ffi::OsString]) -> Option<JsonV1Command> {
    if !args
        .iter()
        .any(|arg| arg == "--format" || arg == "--format=json-v1")
        || !args
            .iter()
            .any(|arg| arg == "json-v1" || arg == "--format=json-v1")
    {
        return None;
    }
    let words = args
        .iter()
        .filter_map(|arg| arg.to_str())
        .collect::<Vec<_>>();
    match words.get(1..)? {
        ["add", ..] => Some(JsonV1Command::Add),
        ["check", ..] => Some(JsonV1Command::Check),
        ["source", "prepare", ..] => Some(JsonV1Command::SourcePrepare),
        ["source", "write-draft", ..] => Some(JsonV1Command::SourceWriteDraft),
        _ => None,
    }
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
