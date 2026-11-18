//! Dedicated synthetic one-case controller process; no background worker or Owner HTTP service.
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = match borrowed.as_slice() {
        [] => nova_review::structured_acceptance::serve(),
        ["--describe"] => {
            nova_review::structured_acceptance::metadata().map(|metadata| println!("{metadata}"))
        }
        ["--profile", profile] => nova_review::structured_acceptance::serve_for_profile(profile),
        ["--profile", profile, "--describe"] | ["--describe", "--profile", profile] => {
            nova_review::structured_acceptance::metadata_for_profile(profile)
                .map(|metadata| println!("{metadata}"))
        }
        _ => Err(nova_core::NovaError::Validation(
            "structured_acceptance: arguments_invalid".into(),
        )),
    };
    if outcome.is_err() {
        eprintln!("structured acceptance stopped");
        std::process::exit(1);
    }
}
