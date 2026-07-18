use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use mko_core::{
    approve::{ApproveSourceRequest, approve_source},
    check::{CheckRequest, check_repository},
    hooks::install_hooks,
    model::AssetStatus,
    pdf::{ExtractionWorkerResponse, extract_pdf_pages_from_reader, worker_executable},
    prepare::{PrepareRequest, prepare_source},
    registry::{
        AssetOperationRequest, CaptureRequest, accept_changed_asset, capture_asset, inspect_asset,
        repair_lineage,
    },
    source::{
        RepairSourceStateRequest, WriteSourceRequest, repair_source_state, write_source_draft,
    },
};

#[derive(Parser)]
#[command(name = "mko", version, about = "My Knowledge OS deterministic core")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    repo: PathBuf,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    staged: bool,
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
struct WriteDraftArgs {
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

fn main() {
    let json_requested = std::env::args_os().any(|argument| argument == "--json");
    match Cli::try_parse() {
        Ok(cli) => {
            let json = cli_requests_json(&cli);
            let check_requested = matches!(&cli.command, Command::Check(_));
            match run(cli) {
                Ok(RunOutcome::Success) => {}
                Ok(RunOutcome::ValidationFailed) => std::process::exit(1),
                Err(error) => {
                    emit_error(error.code(), error.message(), json);
                    std::process::exit(if check_requested { 2 } else { 1 });
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
            emit_error("usage", &error.to_string(), json_requested);
            std::process::exit(2);
        }
    }
}

fn cli_requests_json(cli: &Cli) -> bool {
    matches!(
        &cli.command,
        Command::Asset {
            command: AssetCommand::Capture(CaptureArgs { json: true, .. })
                | AssetCommand::Inspect(AssetOperationArgs { json: true, .. })
                | AssetCommand::AcceptChange(AssetOperationArgs { json: true, .. })
                | AssetCommand::RepairLineage(AssetOperationArgs { json: true, .. }),
        } | Command::Source {
            command: SourceCommand::WriteDraft(WriteDraftArgs { json: true, .. }),
        } | Command::Source {
            command: SourceCommand::RepairState(RepairStateArgs { json: true, .. }),
        } | Command::Check(CheckArgs { json: true, .. })
            | Command::Human {
                command: HumanCommand::ApproveSource(ApproveSourceArgs { json: true, .. }),
            }
            | Command::Hooks {
                command: HooksCommand::Install(HookInstallArgs { json: true, .. }),
            }
    )
}

fn emit_error(code: &str, message: &str, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "result": "error",
                "error": {
                    "code": code,
                    "message": message,
                },
            })
        );
    } else {
        eprintln!("{code}: {message}");
    }
}

enum RunOutcome {
    Success,
    ValidationFailed,
}

fn run(cli: Cli) -> Result<RunOutcome, mko_core::error::MkoError> {
    match cli.command {
        Command::Asset {
            command: AssetCommand::Capture(arguments),
        } => capture(arguments).map(|_| RunOutcome::Success),
        Command::Asset {
            command: AssetCommand::Inspect(arguments),
        } => inspect(arguments).map(|_| RunOutcome::Success),
        Command::Asset {
            command: AssetCommand::AcceptChange(arguments),
        } => accept_change(arguments).map(|_| RunOutcome::Success),
        Command::Asset {
            command: AssetCommand::RepairLineage(arguments),
        } => repair_asset_lineage(arguments).map(|_| RunOutcome::Success),
        Command::Check(arguments) => check(arguments),
        Command::Source {
            command: SourceCommand::Prepare(arguments),
        } => prepare(arguments).map(|_| RunOutcome::Success),
        Command::Source {
            command: SourceCommand::WriteDraft(arguments),
        } => write_draft(arguments).map(|_| RunOutcome::Success),
        Command::Source {
            command: SourceCommand::RepairState(arguments),
        } => repair_source(arguments).map(|_| RunOutcome::Success),
        Command::ExtractPdf => extract_pdf().map(|_| RunOutcome::Success),
        Command::Human {
            command: HumanCommand::ApproveSource(arguments),
        } => approve(arguments).map(|_| RunOutcome::Success),
        Command::Hooks {
            command: HooksCommand::Install(arguments),
        } => install_hook(arguments).map(|_| RunOutcome::Success),
    }
}

fn write_draft(arguments: WriteDraftArgs) -> Result<(), mko_core::error::MkoError> {
    let response = std::fs::read(&arguments.response).map_err(|error| {
        mko_core::error::MkoError::new(
            "semantic_response_unreadable",
            format!("cannot read {}: {error}", arguments.response.display()),
        )
    })?;
    let request = WriteSourceRequest::new(&arguments.repo, &arguments.bundle, response)
        .with_slug(arguments.slug)
        .with_replace_pending(arguments.replace_pending)
        .with_clear_stale_lock(arguments.clear_stale_lock);
    let result = write_source_draft(request)?;
    if arguments.json {
        println!(
            "{}",
            serde_json::json!({
                "result": result.result,
                "source_id": result.source_id,
                "source_path": result.source_path,
                "content_revision": result.content_revision,
            })
        );
    } else {
        println!(
            "{} {} {} {}",
            result.result, result.source_id, result.source_path, result.content_revision
        );
    }
    Ok(())
}

fn repair_source(arguments: RepairStateArgs) -> Result<(), mko_core::error::MkoError> {
    let result = repair_source_state(
        RepairSourceStateRequest::new(&arguments.repo, &arguments.asset_id)
            .with_clear_stale_lock(arguments.clear_stale_lock),
    )?;
    if arguments.json {
        println!(
            "{}",
            serde_json::json!({
                "result": result.result,
                "source_id": result.source_id,
                "asset_id": result.asset_id,
            })
        );
    } else {
        println!("{} {} {}", result.result, result.source_id, result.asset_id);
    }
    Ok(())
}

fn prepare(arguments: PrepareArgs) -> Result<(), mko_core::error::MkoError> {
    let mut request = PrepareRequest::new(&arguments.repo, &arguments.asset_id, &arguments.output)
        .with_clear_stale_lock(arguments.clear_stale_lock);
    if let Some(local_config) = arguments.local_config {
        request = request.with_local_config(local_config);
    }
    let bundle = prepare_source(request, &worker_executable()?)?;
    println!("prepared {} {}", bundle.asset_id, bundle.source_id);
    Ok(())
}

fn extract_pdf() -> Result<(), mko_core::error::MkoError> {
    let response = match extract_pdf_pages_from_reader(std::io::stdin().lock()) {
        Ok(pages) => ExtractionWorkerResponse::Success { pages },
        Err(error) => ExtractionWorkerResponse::Error {
            code: error.code().into(),
            message: error.message().into(),
        },
    };
    let output = serde_json::to_string(&response).map_err(|error| {
        mko_core::error::MkoError::new("pdf_extraction_failed", error.to_string())
    })?;
    println!("{output}");
    Ok(())
}

fn capture(arguments: CaptureArgs) -> Result<(), mko_core::error::MkoError> {
    let mut request = CaptureRequest::new(arguments.repo, arguments.file)
        .with_title(arguments.title)
        .with_domains(arguments.domains)
        .with_slug(arguments.slug);
    if let Some(local_config) = arguments.local_config {
        request = request.with_local_config(local_config);
    }
    let result = capture_asset(request)?;
    if arguments.json {
        println!(
            "{}",
            serde_json::json!({
                "result": result.result,
                "asset_id": result.asset_id,
                "registry_path": result.registry_path,
            })
        );
    } else {
        println!(
            "{} {} {}",
            result.result, result.asset_id, result.registry_path
        );
    }
    Ok(())
}

fn inspect(arguments: AssetOperationArgs) -> Result<(), mko_core::error::MkoError> {
    let json = arguments.json;
    let asset = inspect_asset(operation_request(&arguments))?;
    let status = asset_status_name(&asset.asset_status);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "result": status,
                "asset_id": asset.id,
            })
        );
    } else {
        println!("{status} {}", asset.id);
    }
    Ok(())
}

fn accept_change(arguments: AssetOperationArgs) -> Result<(), mko_core::error::MkoError> {
    let json = arguments.json;
    let asset = accept_changed_asset(operation_request(&arguments))?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "result": "accepted",
                "asset_id": asset.id,
                "supersedes": asset.supersedes,
            })
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

fn repair_asset_lineage(arguments: AssetOperationArgs) -> Result<(), mko_core::error::MkoError> {
    let json = arguments.json;
    let asset_id = arguments.asset_id.clone();
    repair_lineage(operation_request(&arguments))?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "result": "repaired",
                "asset_id": asset_id,
            })
        );
    } else {
        println!("repaired {asset_id}");
    }
    Ok(())
}

fn check(arguments: CheckArgs) -> Result<RunOutcome, mko_core::error::MkoError> {
    let report =
        check_repository(CheckRequest::new(&arguments.repo).with_staged(arguments.staged))?;
    if arguments.json {
        println!(
            "{}",
            serde_json::to_string(&report).map_err(|error| {
                mko_core::error::MkoError::new("check_failed", error.to_string())
            })?
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
        RunOutcome::Success
    } else {
        RunOutcome::ValidationFailed
    })
}

fn approve(arguments: ApproveSourceArgs) -> Result<(), mko_core::error::MkoError> {
    let result = approve_source(ApproveSourceRequest::new(
        &arguments.repo,
        &arguments.source_id,
    ))?;
    if arguments.json {
        println!(
            "{}",
            serde_json::json!({
                "result": "approved",
                "source_id": result.source_id,
                "source_path": result.source_path,
                "revision": result.revision,
            })
        );
    } else {
        println!("approved {} {}", result.source_id, result.revision);
    }
    Ok(())
}

fn install_hook(arguments: HookInstallArgs) -> Result<(), mko_core::error::MkoError> {
    let result = install_hooks(&arguments.repo)?;
    if arguments.json {
        println!(
            "{}",
            serde_json::json!({
                "result": result.result,
                "hook_path": result.hook_path,
            })
        );
    } else {
        println!("{} {}", result.result, result.hook_path);
    }
    Ok(())
}

fn operation_request(arguments: &AssetOperationArgs) -> AssetOperationRequest {
    let mut request = AssetOperationRequest::new(&arguments.repo, &arguments.asset_id)
        .with_clear_stale_lock(arguments.clear_stale_lock);
    if let Some(local_config) = &arguments.local_config {
        request = request.with_local_config(local_config);
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
