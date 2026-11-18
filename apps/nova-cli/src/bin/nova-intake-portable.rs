//! Local restoration utility. It cannot activate a backup or contact any service.
use clap::Parser;
use nova_core::{VaultKey, intake::restore_intake_portable_file};
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Restore an encrypted AI Native export into a NEW quarantined local directory")]
struct Args {
    #[arg(long)]
    package: PathBuf,
    #[arg(long)]
    export_key_file: PathBuf,
    #[arg(long)]
    destination: PathBuf,
}
fn main() {
    let args = Args::parse();
    let result = (|| -> nova_core::Result<_> {
        let file = std::fs::File::open(&args.export_key_file)?;
        let info = file.metadata()?;
        if !info.is_file() || info.len() > 128 {
            return Err(nova_core::NovaError::Validation(
                "export_key_file_size".into(),
            ));
        }
        let mut encoded = zeroize::Zeroizing::new(String::new());
        file.take(129).read_to_string(&mut encoded)?;
        if encoded.len() > 128 {
            return Err(nova_core::NovaError::Validation(
                "export_key_file_size".into(),
            ));
        }
        let key = VaultKey::from_hex(&encoded)?;
        restore_intake_portable_file(&args.package, &key, &args.destination)
    })();
    match result {
        Ok(report) => println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serializable report")
        ),
        Err(e) => {
            eprintln!(
                "{}: portable restore failed; no activation performed",
                e.code()
            );
            std::process::exit(1);
        }
    }
}
