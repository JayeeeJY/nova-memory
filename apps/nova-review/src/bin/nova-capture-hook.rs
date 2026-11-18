//! Explicit local capture consumer. Never discovers or edits Host configuration.
use clap::{Parser, Subcommand};
use nova_review::capture_hook::*;
use std::{io::Read, path::PathBuf};

#[derive(Parser)]
#[command(
    name = "nova-capture-hook",
    version,
    about = "Private local capture relay; installation is a separate owner action"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Setup {
        #[arg(long)]
        bundle_file: PathBuf,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        origin_session_id: Option<String>,
    },
    EnqueueVisible {
        #[arg(long)]
        binding_file: PathBuf,
        #[arg(long)]
        host: String,
        #[arg(long)]
        installation: String,
        #[arg(long)]
        event: String,
    },
    RenderInstall {
        #[arg(long)]
        binding_file: PathBuf,
        #[arg(long)]
        executable: Option<PathBuf>,
    },
    Status {
        #[arg(long)]
        binding_file: PathBuf,
    },
    Flush {
        #[arg(long)]
        binding_file: PathBuf,
    },
    ImportVisible {
        #[arg(long)]
        binding_file: PathBuf,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value_t = 16)]
        max_events: usize,
    },
}
fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::ImportVisible {
            binding_file,
            file,
            max_events,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&import_visible_capture_history(
                &binding_file,
                &file,
                max_events
            )?)?
        ),
        Command::Setup {
            bundle_file,
            directory,
            origin_session_id,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&setup_capture_hook(
                &bundle_file,
                &directory,
                origin_session_id.as_deref()
            )?)?
        ),
        Command::EnqueueVisible {
            binding_file,
            host,
            installation,
            event,
        } => {
            let mut bytes = zeroize::Zeroizing::new(Vec::new());
            std::io::stdin()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let result = consume_capture_hook(&binding_file, &host, &installation, &event, &bytes)?;
            // Hook stdout can become model context. Emit only the neutral empty object.
            println!("{{}}");
            eprintln!(
                "nova-capture: local_queued={} source_acknowledged={} state={:?} delivery_stopped={}",
                result.local_queued,
                result.source_acknowledged,
                result.state,
                result.delivery_stopped
            );
        }
        Command::RenderInstall {
            binding_file,
            executable,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&render_capture_hook_plan(
                &binding_file,
                &executable.unwrap_or(std::env::current_exe()?)
            )?)?
        ),
        Command::Status { binding_file } => println!(
            "{}",
            serde_json::to_string_pretty(&capture_hook_status(&binding_file, false)?)?
        ),
        Command::Flush { binding_file } => println!(
            "{}",
            serde_json::to_string_pretty(&capture_hook_status(&binding_file, true)?)?
        ),
    }
    Ok(())
}
fn main() {
    if run(Cli::parse().command).is_err() {
        // Never print credential, incoming content or remote error bodies; never exit 2 to block a Host turn.
        eprintln!(
            "nova-capture: operation failed; no Source ACK is claimed. Check the private local queue status."
        );
        std::process::exit(1);
    }
}
