//! Local credential boundary. A credential binds configured policy, not process attestation.
use crate::generic::GenericMcpServer;
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;
#[derive(Clone)]
pub struct ClientCredential(String);
impl std::fmt::Debug for ClientCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientCredential([REDACTED])")
    }
}
impl ClientCredential {
    pub fn read(path: &std::path::Path) -> std::io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.len() > 256 {
            return Err(std::io::Error::other(
                "credential must be a regular small file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(std::io::Error::other("credential must be owner-only"));
            }
        }
        let value = std::fs::read_to_string(path)?;
        let value = value.trim();
        if !(32..=128).contains(&value.len())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(std::io::Error::other("invalid client credential format"));
        }
        Ok(Self(value.into()))
    }
    pub fn read_for_vault(
        path: &std::path::Path,
        key_path: &std::path::Path,
    ) -> std::io::Result<Self> {
        let credential = Self::read(path)?;
        let key_value = std::fs::read_to_string(key_path)?;
        if credential.0 == key_value.trim() {
            return Err(std::io::Error::other(
                "client credential must differ from the master key",
            ));
        }
        Ok(credential)
    }
    pub(crate) fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(self.0.as_bytes()))
    }
    pub fn permits(&self, header: Option<&str>) -> bool {
        let Some(value) = header.and_then(|s| s.strip_prefix("Bearer ")) else {
            return false;
        };
        let mut diff = self.0.len() ^ value.len();
        for (a, b) in self.0.bytes().zip(value.bytes()) {
            diff |= usize::from(a ^ b);
        }
        diff == 0
    }
}
#[derive(Clone)]
pub enum CredentialPolicy {
    Static(ClientCredential),
    Registry(crate::registry::RegistryCredential),
}
impl CredentialPolicy {
    fn permits(&self, header: Option<&str>) -> bool {
        match self {
            Self::Static(c) => c.permits(header),
            Self::Registry(c) => c.permits(header),
        }
    }
}
#[derive(Clone)]
struct Boundary {
    credential: CredentialPolicy,
    authority: String,
}
async fn guard(State(state): State<Boundary>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let has_origin = request.headers().contains_key(header::ORIGIN);
    let site = request
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok());
    let forbidden = has_origin || matches!(site, Some("cross-site" | "same-site"));
    let valid = host == Some(state.authority.as_str())
        && state.credential.permits(
            request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
        );
    let deleting = request.method() == axum::http::Method::DELETE;
    let mut response = if forbidden {
        (StatusCode::FORBIDDEN, "NOVA_ORIGIN_FORBIDDEN").into_response()
    } else if valid {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "NOVA_CLIENT_UNAUTHORIZED").into_response()
    };
    // rmcp 3.1.2 closes legacy sessions successfully but returns 202. Python
    // SDK 1.26 expects 200/204 for DELETE. Normalize only successful DELETE,
    // never the mandatory 202 response to POST notifications.
    if deleting && response.status() == StatusCode::ACCEPTED {
        *response.status_mut() = StatusCode::NO_CONTENT;
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}
pub fn generic_router(
    server: GenericMcpServer,
    credential: ClientCredential,
    authority: String,
    cancel: CancellationToken,
) -> Router {
    let service: StreamableHttpService<GenericMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(server.clone()),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(true)
                .with_max_request_body_bytes(96 * 1024)
                .with_sse_keep_alive(None)
                .with_cancellation_token(cancel),
        );
    Router::new()
        .nest_service("/mcp", service)
        .layer(axum::extract::DefaultBodyLimit::max(96 * 1024))
        .layer(middleware::from_fn_with_state(
            Boundary {
                credential: CredentialPolicy::Static(credential),
                authority,
            },
            guard,
        ))
}

pub fn protected_router<S>(
    server: S,
    credential: CredentialPolicy,
    authority: String,
    cancel: CancellationToken,
) -> Router
where
    S: rmcp::ServerHandler + Clone + Send + 'static,
{
    let service: StreamableHttpService<S, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(server.clone()),
        Default::default(),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(true)
            .with_max_request_body_bytes(96 * 1024)
            .with_sse_keep_alive(None)
            .with_cancellation_token(cancel),
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(
            Boundary {
                credential,
                authority,
            },
            guard,
        ))
}
