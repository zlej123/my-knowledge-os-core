use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mko", version, about = "My Knowledge OS deterministic core")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Asset,
    Source,
    Check,
    Human,
    Hooks,
}

fn main() {
    let _ = Cli::parse();
}
