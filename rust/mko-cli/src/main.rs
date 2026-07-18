use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use mko_core::registry::{CaptureRequest, capture_asset};

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
    Source,
    Check,
    Human,
    Hooks,
}

#[derive(Subcommand)]
enum AssetCommand {
    Capture(CaptureArgs),
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

fn main() {
    let json_requested = std::env::args_os().any(|argument| argument == "--json");
    match Cli::try_parse() {
        Ok(cli) => {
            let json = cli_requests_json(&cli);
            if let Err(error) = run(cli) {
                emit_error(error.code(), error.message(), json);
                std::process::exit(1);
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
            command: AssetCommand::Capture(CaptureArgs { json: true, .. }),
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

fn run(cli: Cli) -> Result<(), mko_core::error::MkoError> {
    match cli.command {
        Command::Asset {
            command: AssetCommand::Capture(arguments),
        } => capture(arguments),
        Command::Source | Command::Check | Command::Human | Command::Hooks => Ok(()),
    }
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
