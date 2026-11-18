//! Owner-managed local registry, never populated from MCP clientInfo or tool input.
use crate::{service::LocalClientBinding, transport::ClientCredential};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub owner_id: String,
    pub space_id: String,
    pub client_id: String,
    pub capabilities: Vec<String>,
}
impl Principal {
    pub fn validate(&self) -> io::Result<()> {
        for value in [&self.owner_id, &self.space_id, &self.client_id] {
            if value.is_empty()
                || value.len() > 128
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
            {
                return Err(io::Error::other("invalid principal identifier"));
            }
        }
        if self.capabilities.is_empty()
            || self
                .capabilities
                .iter()
                .any(|c| !matches!(c.as_str(), "capture" | "query"))
            || self
                .capabilities
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.capabilities.len()
        {
            return Err(io::Error::other("invalid capabilities"));
        }
        Ok(())
    }
    pub fn binding(&self) -> io::Result<LocalClientBinding> {
        self.validate()?;
        Ok(LocalClientBinding::registered(
            self.owner_id.clone(),
            self.space_id.clone(),
            self.client_id.clone(),
            self.capabilities.clone(),
        ))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    principal: Principal,
    token_file: PathBuf,
    expires_at: String,
    revoked: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema_version: String,
    clients: Vec<Entry>,
}
#[derive(Clone)]
pub struct RegistryCredential {
    path: PathBuf,
    selected: String,
    expected: Principal,
    key_path: PathBuf,
}
impl RegistryCredential {
    pub fn load(path: &Path, selected: &str, key_path: &Path) -> io::Result<(Self, Principal)> {
        let (principal, _) = resolve(path, selected, key_path)?;
        Ok((
            Self {
                path: path.into(),
                selected: selected.into(),
                expected: principal.clone(),
                key_path: key_path.into(),
            },
            principal,
        ))
    }
    pub fn permits(&self, header: Option<&str>) -> bool {
        resolve(&self.path, &self.selected, &self.key_path)
            .is_ok_and(|(p, c)| p == self.expected && c.permits(header))
    }
}
fn resolve(
    path: &Path,
    selected: &str,
    key_path: &Path,
) -> io::Result<(Principal, ClientCredential)> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > 65536 {
        return Err(io::Error::other("registry must be a small regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other("registry must be owner-only"));
        }
    }
    let data: Registry = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| io::Error::other("invalid registry"))?;
    if data.schema_version != "nova.clients.v0.1"
        || data.clients.is_empty()
        || data.clients.len() > 64
    {
        return Err(io::Error::other("invalid registry version or size"));
    }
    let mut clients = std::collections::HashSet::new();
    let mut tokens = std::collections::HashSet::new();
    let mut found = None;
    for entry in data.clients {
        entry.principal.validate()?;
        if !clients.insert(entry.principal.client_id.clone()) || !entry.token_file.is_absolute() {
            return Err(io::Error::other(
                "duplicate client or non-absolute credential path",
            ));
        }
        let credential = ClientCredential::read_for_vault(&entry.token_file, key_path)?;
        if !tokens.insert(credential.fingerprint()) {
            return Err(io::Error::other("client credentials must be distinct"));
        }
        let expiry = OffsetDateTime::parse(&entry.expires_at, &Rfc3339)
            .map_err(|_| io::Error::other("invalid credential expiry"))?;
        if entry.principal.client_id == selected
            && !entry.revoked
            && expiry > OffsetDateTime::now_utc()
        {
            found = Some((entry.principal, credential));
        }
    }
    found.ok_or_else(|| io::Error::other("client unavailable or expired/revoked"))
}
