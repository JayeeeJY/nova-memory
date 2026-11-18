//! Local browser entry preference. Never exposes the Owner key or authorizes MCP.
use super::*;
use axum::{body::Bytes, http::HeaderMap};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path, sync::Mutex};
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preference {
    schema_version: String,
    owner: String,
    space: String,
    pub revision: u64,
    pub credential_unlock_enabled: bool,
}
struct EntryState {
    preference: Preference,
    session: Zeroizing<String>,
}
pub(super) struct LocalAccess {
    path: PathBuf,
    cookie_name: String,
    state: Mutex<EntryState>,
}
fn invalid() -> NovaError {
    NovaError::InvalidState("本机打开方式设置不可用，请检查资料目录。".into())
}
impl LocalAccess {
    pub fn open(store: &Path, owner: &str, space: &str, port: u16) -> Result<Self, NovaError> {
        let filename = store
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(invalid)?;
        let path = store.with_file_name(format!("{filename}.workspace-access.json"));
        let default = Preference {
            schema_version: "nova.workspace-access.v0.1".into(),
            owner: owner.into(),
            space: space.into(),
            revision: 1,
            credential_unlock_enabled: false,
        };
        let preference = Self::read(&path, &default)?;
        Ok(Self {
            path,
            cookie_name: format!("nova_workspace_session_{port}"),
            state: Mutex::new(EntryState {
                preference,
                session: Zeroizing::new(VaultKey::generate()?.to_hex()),
            }),
        })
    }
    fn read(path: &Path, expected: &Preference) -> Result<Preference, NovaError> {
        use std::fs::OpenOptions;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A disappeared saved preference must not silently disable a lock.
                if expected.revision != 1 || expected.credential_unlock_enabled {
                    return Err(invalid());
                }
                return Ok(expected.clone());
            }
            Err(_) => return Err(invalid()),
        };
        let metadata = file.metadata().map_err(|_| invalid())?;
        if !metadata.is_file() || metadata.len() > 4096 {
            return Err(invalid());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o777 != 0o600 || metadata.uid() != unsafe { libc::geteuid() } {
                return Err(invalid());
            }
        }
        let mut bytes = Vec::new();
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > 4096 {
            return Err(invalid());
        }
        let preference: Preference = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if preference.schema_version != expected.schema_version
            || preference.owner != expected.owner
            || preference.space != expected.space
            || preference.revision == 0
            || preference.revision < expected.revision
        {
            return Err(invalid());
        }
        Ok(preference)
    }
    fn refresh(&self, state: &mut EntryState) -> Result<(), NovaError> {
        let current = Self::read(&self.path, &state.preference)?;
        if current.revision != state.preference.revision
            || current.credential_unlock_enabled != state.preference.credential_unlock_enabled
        {
            state.session = Zeroizing::new(VaultKey::generate()?.to_hex());
            state.preference = current;
        }
        Ok(())
    }
    pub fn preference(&self) -> Result<Preference, NovaError> {
        let mut state = self.state.lock().map_err(|_| invalid())?;
        self.refresh(&mut state)?;
        Ok(state.preference.clone())
    }
    pub fn permits_cookie(&self, headers: &HeaderMap) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if self.refresh(&mut state).is_err() || state.preference.credential_unlock_enabled {
            return false;
        }
        let mut values = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .flat_map(|h| h.split(';'))
            .filter_map(|part| part.trim().split_once('='))
            .filter(|(name, _)| *name == self.cookie_name)
            .map(|(_, value)| value);
        let Some(value) = values.next() else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        let expected = state.session.as_bytes();
        let mut diff = expected.len() ^ value.len();
        for (a, b) in expected.iter().zip(value.bytes()) {
            diff |= usize::from(*a ^ b);
        }
        diff == 0
    }
    fn cookie(&self) -> Result<String, NovaError> {
        let mut state = self.state.lock().map_err(|_| invalid())?;
        self.refresh(&mut state)?;
        if state.preference.credential_unlock_enabled {
            return Err(NovaError::PolicyDenied("请使用解锁凭据打开工作台。".into()));
        }
        Ok(format!(
            "{}={}; Path=/console/; HttpOnly; SameSite=Strict",
            self.cookie_name,
            state.session.as_str()
        ))
    }
    fn save(&self, expected: u64, enabled: bool) -> Result<Preference, NovaError> {
        use std::io::Write;
        let mut state = self.state.lock().map_err(|_| invalid())?;
        self.refresh(&mut state)?;
        if state.preference.revision != expected {
            return Err(NovaError::InvalidState("设置已更新，请刷新后重试。".into()));
        }
        if state.preference.credential_unlock_enabled == enabled {
            return Ok(state.preference.clone());
        }
        let mut next = state.preference.clone();
        next.revision = next.revision.checked_add(1).ok_or_else(invalid)?;
        next.credential_unlock_enabled = enabled;
        let mut file = tempfile::NamedTempFile::new_in(self.path.parent().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| invalid())?;
        }
        file.write_all(&serde_json::to_vec(&next)?)
            .map_err(|_| invalid())?;
        file.as_file().sync_all().map_err(|_| invalid())?;
        file.persist(&self.path).map_err(|_| invalid())?;
        state.session = Zeroizing::new(VaultKey::generate()?.to_hex());
        state.preference = next.clone();
        Ok(next)
    }
}
pub(super) fn routes() -> Router<ReviewServer> {
    Router::new()
        .route("/console/local-session", post(browser_session))
        .route(
            "/console/api/owner/intake/workspace-access",
            get(settings).post(save_settings),
        )
}
async fn browser_session(
    State(s): State<ReviewServer>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let result = (|| {
        if bytes.as_ref() != b"{}"
            || headers.get("x-nova-review").and_then(|v| v.to_str().ok()) != Some("owner-review")
        {
            return Err(NovaError::PolicyDenied("请从本机工作台打开。".into()));
        }
        s.local_access.as_ref().ok_or_else(invalid)?.cookie()
    })();
    match result {
        Ok(cookie) => {
            ([(header::SET_COOKIE, cookie)], Json(json!({"opened":true}))).into_response()
        }
        Err(error) => failure(error),
    }
}
async fn settings(State(s): State<ReviewServer>) -> Response {
    intake::response((|| {
        Ok(json!({"settings":s.local_access.as_ref().ok_or_else(invalid)?.preference()?}))
    })())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    expected_revision: u64,
    credential_unlock_enabled: bool,
    owner_credential: Option<String>,
}
impl Drop for Change {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.owner_credential.zeroize();
    }
}
async fn save_settings(
    State(s): State<ReviewServer>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let _guard = s.lock.lock().await;
    intake::response((|| {
        let input: Change = serde_json::from_slice(&bytes)
            .map_err(|_| NovaError::Validation("打开方式设置无效。".into()))?;
        let token = s.owner_token.as_ref().ok_or_else(invalid)?;
        if input.credential_unlock_enabled {
            let from_header = token.permits(
                headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok()),
            );
            let from_input = input
                .owner_credential
                .as_ref()
                .is_some_and(|value| token.permits(Some(&format!("Bearer {}", value.trim()))));
            if !from_header && !from_input {
                return Err(NovaError::PolicyDenied(
                    "请核对用于解锁的独立凭据后开启。".into(),
                ));
            }
        }
        Ok(
            json!({"settings":s.local_access.as_ref().ok_or_else(invalid)?.save(input.expected_revision,input.credential_unlock_enabled)?}),
        )
    })())
}
