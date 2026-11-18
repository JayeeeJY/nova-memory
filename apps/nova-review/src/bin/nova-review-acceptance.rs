//! Dedicated opt-in synthetic acceptance process. It never starts the ordinary HTTP service.
use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
struct Cli {
    #[arg(long)]
    profile: Option<PathBuf>,
    #[arg(long, conflicts_with = "profile")]
    metadata: bool,
}
fn main() {
    let args = Cli::parse();
    if args.metadata {
        println!("{}", nova_review::acceptance::metadata());
        return;
    }
    if args.profile.is_none() || nova_review::acceptance::serve(args.profile.unwrap()).is_err() {
        // All diagnostics are intentionally fixed: no paths, provider bodies or credentials.
        eprintln!("acceptance process stopped");
        std::process::exit(1);
    }
}
