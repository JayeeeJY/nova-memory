//! A bounded stdio adapter to an explicitly selected local v0.3 connection, never an Owner client.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use zeroize::{Zeroize, Zeroizing};

const VERSION: &str = "nova.local-mcp-connection.v0.1";
const MAX_CONFIG: u64 = 4096;
pub(crate) const MAX_REQUEST: u64 = 96 * 1024;
pub(crate) const MAX_RESPONSE: usize = 256 * 1024;
// This adapter implements the initialized stdio lifecycle, not newer HTTP inline metadata.
const BRIDGE_PROTOCOL: &str = "2025-11-25";
const CONFIG_INVALIDATED: &str = "NOVA_CONNECTION_CONFIG_INVALIDATED: 连接配置已被移除、修改或不可访问，可能已撤销。未执行本次操作。请在 Nova 工作台检查并重新连接；不要自动重试或切换凭据。";
const NOT_AUTHORIZED: &str = "NOVA_CONNECTION_NOT_AUTHORIZED: 连接未获授权，可能已撤销、过期或凭据不匹配。未执行本次操作。请在 Nova 工作台检查并重新连接；不要自动重试或切换凭据。";

#[derive(Debug)]
struct AuthorizationDenied;
impl std::fmt::Display for AuthorizationDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NOVA_BRIDGE_AUTHORIZATION_DENIED")
    }
}
impl std::error::Error for AuthorizationDenied {}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalConnection {
    pub schema_version: String,
    pub connection_id: String,
    pub endpoint: String,
    pub token: String,
}
impl Drop for LocalConnection {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}
impl LocalConnection {
    pub fn new(connection_id: String, endpoint: String, token: String) -> Self {
        Self {
            schema_version: VERSION.into(),
            connection_id,
            endpoint,
            token,
        }
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == VERSION,
            "NOVA_BRIDGE_UNSUPPORTED_CONFIG"
        );
        ensure!(
            !self.connection_id.is_empty()
                && self.connection_id.len() <= 128
                && self
                    .connection_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "NOVA_BRIDGE_INVALID_CONNECTION"
        );
        let url = url::Url::parse(&self.endpoint)
            .map_err(|_| anyhow::anyhow!("NOVA_BRIDGE_INVALID_ENDPOINT"))?;
        let ip = url
            .host_str()
            .unwrap_or_default()
            .trim_matches(['[', ']'])
            .parse::<IpAddr>();
        ensure!(
            url.scheme() == "http"
                && ip.is_ok_and(|ip| ip.is_loopback())
                && url.port().is_some_and(|p| p != 0)
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/mcp/v03"
                && url.query().is_none()
                && url.fragment().is_none(),
            "NOVA_BRIDGE_LOOPBACK_ONLY"
        );
        ensure!(
            (32..=128).contains(&self.token.len())
                && self
                    .token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "NOVA_BRIDGE_INVALID_CREDENTIAL"
        );
        Ok(())
    }
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir()
            && !m.file_type().is_symlink()
            && m.uid() == unsafe { libc::geteuid() }
            && m.mode() & 0o077 == 0,
        "NOVA_BRIDGE_PRIVATE_DIRECTORY_REQUIRED"
    );
    Ok(())
}

/// This file is the only secret-bearing artifact. Generated client command/args contain no token.
#[cfg(unix)]
pub fn write_local_connection(directory: &Path, config: &LocalConnection) -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    config.validate()?;
    ensure!(
        directory.is_absolute(),
        "NOVA_BRIDGE_ABSOLUTE_PATH_REQUIRED"
    );
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    private_directory(directory)?;
    let path = directory.join(format!("{}.json", config.connection_id));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    let body = Zeroizing::new(serde_json::to_vec(config)?);
    if file.write_all(&body).and_then(|_| file.sync_all()).is_err() {
        drop(file);
        let _ = fs::remove_file(&path);
        bail!("NOVA_BRIDGE_CONFIG_WRITE_FAILED");
    }
    Ok(path)
}

#[cfg(unix)]
pub fn read_local_connection(path: &Path) -> Result<LocalConnection> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    ensure!(path.is_absolute(), "NOVA_BRIDGE_ABSOLUTE_PATH_REQUIRED");
    private_directory(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("NOVA_BRIDGE_INVALID_PATH"))?,
    )?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file()
            && before.uid() == unsafe { libc::geteuid() }
            && before.mode() & 0o077 == 0
            && before.nlink() == 1
            && before.len() <= MAX_CONFIG,
        "NOVA_BRIDGE_PRIVATE_FILE_REQUIRED"
    );
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_CONFIG + 1).read_to_end(&mut bytes)?;
    let after = fs::symlink_metadata(path)?;
    ensure!(
        bytes.len() as u64 <= MAX_CONFIG
            && before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && !after.file_type().is_symlink()
            && after.nlink() == 1
            && after.mode() & 0o077 == 0,
        "NOVA_BRIDGE_CONFIG_CHANGED"
    );
    let config: LocalConnection = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("NOVA_BRIDGE_INVALID_CONFIG"))?;
    config.validate()?;
    Ok(config)
}

#[cfg(not(unix))]
pub fn read_local_connection(_path: &Path) -> Result<LocalConnection> {
    bail!("NOVA_BRIDGE_UNSUPPORTED_PLATFORM")
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

pub(crate) async fn forward(
    client: &reqwest::Client,
    config: &LocalConnection,
    request: &Value,
    version: Option<&str>,
) -> Result<Option<Value>> {
    let mut send = client
        .post(&config.endpoint)
        .bearer_auth(&config.token)
        .header("Accept", "application/json, text/event-stream")
        .json(request);
    if let Some(v) = version {
        send = send.header("MCP-Protocol-Version", v);
    }
    let mut response = send
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("NOVA_BRIDGE_TRANSPORT_FAILED"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(AuthorizationDenied.into());
    }
    ensure!(
        response.status().is_success(),
        "NOVA_BRIDGE_CONNECTION_UNAVAILABLE"
    );
    if response.status() == 202 || response.status() == 204 {
        return Ok(None);
    }
    ensure!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').next() == Some("application/json")),
        "NOVA_BRIDGE_JSON_REQUIRED"
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= MAX_RESPONSE as u64),
        "NOVA_BRIDGE_RESPONSE_LIMIT"
    );
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("NOVA_BRIDGE_TRANSPORT_FAILED"))?
    {
        ensure!(
            body.len() + chunk.len() <= MAX_RESPONSE,
            "NOVA_BRIDGE_RESPONSE_LIMIT"
        );
        body.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| anyhow::anyhow!("NOVA_BRIDGE_INVALID_RESPONSE"))?;
    ensure!(
        value.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && value.get("id") == request.get("id")
            && (value.get("result").is_some() != value.get("error").is_some()),
        "NOVA_BRIDGE_INVALID_RESPONSE"
    );
    Ok(Some(value))
}

pub(crate) fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(45))
        .build()?)
}

/// One upstream attempt per input; no redirects, proxy, automatic retries, shell, or new authority.
pub async fn run<R, W>(path: &Path, input: R, mut output: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let pinned = read_local_connection(path)?;
    let client = http_client()?;
    let mut input = BufReader::new(input);
    let mut version: Option<String> = None;
    let mut initialized = false;
    // Explain terminal invalidation over JSON-RPC instead of only closing stdout.
    // Once invalidated, this process must never resume forwarding, even if a file is restored.
    let mut invalidated: Option<&'static str> = None;
    loop {
        let mut line = Zeroizing::new(Vec::new());
        let size = (&mut input)
            .take(MAX_REQUEST + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if size == 0 {
            break;
        }
        ensure!(size as u64 <= MAX_REQUEST, "NOVA_BRIDGE_INPUT_LIMIT");
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let request = serde_json::from_slice::<Value>(&line);
        let response = match request {
            Err(_) => Some(error(Value::Null, -32700, "Invalid JSON-RPC input.")),
            Ok(mut request) => {
                let id = request.get("id").cloned();
                let method_name = request
                    .get("method")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let method = method_name.as_deref();
                if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
                    || method.is_none()
                    || id
                        .as_ref()
                        .is_some_and(|v| !(v.is_string() || v.is_i64() || v.is_u64()))
                    || (method == Some("initialize")
                        && (id.is_none() || !request["params"].is_object()))
                {
                    Some(error(Value::Null, -32600, "Invalid JSON-RPC request."))
                } else if let Some(message) = invalidated.or_else(|| {
                    (!read_local_connection(path).is_ok_and(|current| current == pinned))
                        .then_some(CONFIG_INVALIDATED)
                }) {
                    invalidated = Some(message);
                    id.map(|id| error(id, -32001, message))
                } else if !initialized && method != Some("initialize") {
                    id.map(|id| error(id, -32002, "Initialize the selected Nova connection first."))
                } else {
                    if method == Some("initialize") {
                        initialized = false;
                        version = None;
                        // Negotiate the adapter's supported transport version explicitly. This
                        // does not switch the Nova application schema, service, or credential.
                        request["params"]["protocolVersion"] = json!(BRIDGE_PROTOCOL);
                    }
                    match forward(&client, &pinned, &request, version.as_deref()).await {
                        Ok(Some(reply)) if method == Some("initialize") => {
                            let info = &reply["result"]["serverInfo"];
                            if info["name"] == "nova-memory-automatic" && info["version"] == "0.3.0"
                                && reply["result"]["protocolVersion"] == BRIDGE_PROTOCOL {
                                version = reply["result"]["protocolVersion"].as_str().map(str::to_owned);
                                initialized = version.is_some();
                                Some(reply)
                            } else {
                                Some(error(id.unwrap_or(Value::Null), -32002, "This bridge requires Nova AI Native v0.3; no different service or application contract was tried."))
                            }
                        },
                        Ok(Some(reply)) => if id.is_some() { Some(reply) } else { None },
                        Ok(None) => id.map(|id| error(id, -32001, "Nova returned no response for this request; do not assume the operation succeeded.")),
                        Err(e) if e.is::<AuthorizationDenied>() => {
                            invalidated = Some(NOT_AUTHORIZED);
                            id.map(|id| error(id, -32001, NOT_AUTHORIZED))
                        },
                        Err(_) => id.map(|id| error(id, -32001, "Nova connection unavailable. Check its local service; do not assume this operation succeeded. Retry a save only with identical request_id and content.")),
                    }
                }
            }
        };
        if let Some(response) = response {
            let encoded = Zeroizing::new(serde_json::to_vec(&response)?);
            ensure!(encoded.len() <= MAX_RESPONSE, "NOVA_BRIDGE_RESPONSE_LIMIT");
            output.write_all(&encoded).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
    }
    Ok(())
}
