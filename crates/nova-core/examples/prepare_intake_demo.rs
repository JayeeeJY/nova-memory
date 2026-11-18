//! Creates a NEW private synthetic workspace directory. No personal sources/model calls.
use nova_core::{Vault, VaultKey, compilation::AssetScope, intake::IntakeStore};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("new directory argument required")?,
    );
    std::fs::create_dir(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    let key = VaultKey::generate()?;
    key.write_new_file(&path.join("vault.key"))?;
    VaultKey::generate()?.write_new_file(&path.join("owner.key"))?;
    drop(Vault::create(&path.join("legacy.sqlite"), key.clone())?);
    drop(IntakeStore::create(
        &path.join("intake.sqlite"),
        key,
        AssetScope {
            owner_id: "owner:founder".into(),
            space: "personal".into(),
        },
    )?);
    std::fs::write(
        path.join("SYNTHETIC_M1_FIXTURE"),
        "N1 synthetic local capture workspace; no personal data\n",
    )?;
    println!("Synthetic workspace initialized. Keep its key separately; no secrets printed.");
    Ok(())
}
