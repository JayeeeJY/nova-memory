use super::*;
use nova_core::{
    AssertionOrigin, CaptureChannel, CaptureFidelity, CaptureRequest, CollectionGrantRequest,
    CorrectionRequest, MemoryType, PrivacyLevel, ReviewTier,
};
use nova_mcp::service::{ApplicationService, LocalClientBinding, LocalVaultPort};
use std::{io::Write, path::Path};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureInput {
    request_id: String,
    title: String,
    content: String,
    #[serde(alias = "privacy_level")]
    privacy: PrivacyLevel,
    #[serde(alias = "user_approved")]
    acknowledge: bool,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    memory_type: Option<MemoryType>,
    #[serde(default)]
    structure: Option<nova_core::StructureInput>,
}
pub(super) async fn capture(
    State(s): State<ReviewServer>,
    Json(i): Json<CaptureInput>,
) -> Response {
    if i.request_id.is_empty()
        || i.request_id.len() > 96
        || i.title.trim().is_empty()
        || i.title.len() > 240
        || i.content.trim().is_empty()
        || i.content.len() > 65536
        || i.summary
            .as_ref()
            .is_some_and(|s| s.trim().is_empty() || s.len() > 8000)
    {
        return failure(NovaError::Validation("invalid capture bounds".into()));
    }
    let binding = LocalClientBinding::registered(
        s.owner.clone(),
        s.space.clone(),
        "web-console".into(),
        vec!["capture".into()],
    );
    let service = ApplicationService::new(
        binding,
        Arc::new(LocalVaultPort::new(
            s.vault_path.clone(),
            s.key_path.clone(),
        )),
    );
    let _lock = s.lock.lock().await;
    let result = service.capture_web(
        CaptureRequest {
            structure: i.structure,
            request_id: Some(nova_mcp::generic::request_bound(
                service.binding(),
                "web-save",
                &i.request_id,
            )),
            owner_id: s.owner,
            source_platform: "web-console".into(),
            capture_channel: CaptureChannel::WebAdapter,
            capture_fidelity: CaptureFidelity::F2VisibleTranscript,
            source_content: i.content.clone(),
            title: i.title,
            memory_type: i.memory_type.unwrap_or(MemoryType::Reflection),
            summary: i
                .summary
                .unwrap_or_else(|| i.content.chars().take(2000).collect()),
            decisions: vec![],
            privacy_level: i.privacy,
            proposed_origin: AssertionOrigin::UserSelfReport,
            review_tier: ReviewTier::Attention,
            permission_scope: vec![
                "personal_review".into(),
                "client:web-console".into(),
                format!("space:{}", s.space),
            ],
            purpose_limits: vec!["personal_memory_continuity".into()],
            observed_at: None,
        },
        i.acknowledge,
    );
    match result {Ok((outcome,canonical))=>Json(json!({"status":if outcome.proposal.status==nova_core::ProposalStatus::Pending {"pending_review"} else {"capture_already_reviewed"},"source":outcome.source,"proposal":outcome.proposal,"receipt":outcome.receipt,"canonical_memory_id":canonical})).into_response(),Err(e)=>failure(e)}
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CollectionInput {
    collection_id: Option<String>,
    expected_revision: Option<u64>,
    title: String,
    member_ids: Vec<String>,
    #[serde(alias = "user_approved")]
    acknowledge: bool,
}
pub(super) async fn collections(State(s): State<ReviewServer>) -> Response {
    let _lock = s.lock.lock().await;
    match s.open().and_then(|v| {
        v.verify_audit_chain()?;
        v.owner_collections(&s.owner)
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
pub(super) async fn collection(
    State(s): State<ReviewServer>,
    Json(i): Json<CollectionInput>,
) -> Response {
    if !i.acknowledge {
        return failure(NovaError::Validation(
            "owner acknowledgement required".into(),
        ));
    }
    let _lock = s.lock.lock().await;
    match s.open().and_then(|mut v| {
        v.put_collection(
            &s.owner,
            &s.space,
            i.collection_id.as_deref(),
            i.expected_revision,
            &i.title,
            i.member_ids,
        )
    }) {
        Ok(v) => Json(json!(v)).into_response(),
        Err(e) => failure(e),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GrantInput {
    request: CollectionGrantRequest,
    acknowledge: bool,
}
pub(super) async fn grant(State(s): State<ReviewServer>, Json(i): Json<GrantInput>) -> Response {
    if !i.acknowledge {
        return failure(NovaError::Validation(
            "owner acknowledgement required".into(),
        ));
    }
    let check = read_registry(&s).and_then(|r| {
        let entry = r["clients"]
            .as_array()
            .and_then(|rows| {
                rows.iter().find(|e| {
                    e["principal"]["client_id"] == i.request.client
                        && e["principal"]["owner_id"] == s.owner
                        && e["principal"]["space_id"] == s.space
                        && e["revoked"] == false
                })
            })
            .ok_or_else(|| NovaError::PolicyDenied("registered client required".into()))?;
        let principal: nova_mcp::registry::Principal =
            serde_json::from_value(entry["principal"].clone())?;
        principal.validate()?;
        if !principal.capabilities.contains(&"query".into()) {
            return Err(NovaError::PolicyDenied("query capability required".into()));
        }
        nova_mcp::registry::RegistryCredential::load(
            s.registry.as_ref().unwrap(),
            &i.request.client,
            &s.key_path,
        )?;
        Ok(())
    });
    if let Err(e) = check {
        return failure(e);
    }
    let _lock = s.lock.lock().await;
    match s
        .open()
        .and_then(|mut v| v.issue_collection_grant(&s.owner, i.request))
    {
        Ok(v) => Json(json!(v)).into_response(),
        Err(e) => failure(e),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AssetInput {
    memory_id: String,
    operation: String,
    request_id: String,
    title: Option<String>,
    summary: Option<String>,
    reason: Option<String>,
    #[serde(alias = "user_approved")]
    acknowledge: bool,
}
pub(super) async fn detail(
    State(s): State<ReviewServer>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let _lock = s.lock.lock().await;
    match s.open().and_then(|v| v.owner_asset_detail(&s.owner, &id)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
pub(super) async fn mutate(State(s): State<ReviewServer>, Json(i): Json<AssetInput>) -> Response {
    if !i.acknowledge {
        return failure(NovaError::Validation(
            "owner acknowledgement required".into(),
        ));
    }
    let _lock = s.lock.lock().await;
    match s.open().and_then(|mut v| match i.operation.as_str() {
        "correct" => Ok(json!(v.correct_memory(CorrectionRequest {
            request_id: Some(i.request_id),
            subject: s.owner.clone(),
            memory_id: i.memory_id,
            title: i.title,
            summary: i.summary.unwrap_or_default(),
            reason: i.reason.unwrap_or_default()
        })?)),
        "delete" => Ok(json!(v.delete_memory(&s.owner, &i.memory_id)?)),
        "dispute" => Ok(json!(v.dispute_memory(nova_core::DisputeRequest {
            request_id: Some(i.request_id),
            subject: s.owner.clone(),
            memory_id: i.memory_id,
            reason: i.reason.unwrap_or_default()
        })?)),
        _ => Err(NovaError::Validation("unknown owner operation".into())),
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
pub(super) fn read_registry(s: &ReviewServer) -> Result<serde_json::Value, NovaError> {
    let p = s
        .registry
        .as_ref()
        .ok_or_else(|| NovaError::Validation("configure --registry first".into()))?;
    let m = std::fs::symlink_metadata(p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if m.permissions().mode() & 0o077 != 0 {
            return Err(NovaError::Validation("private registry required".into()));
        }
    }
    if !m.is_file() || m.len() > 65536 {
        return Err(NovaError::Validation("private registry required".into()));
    }
    let r: serde_json::Value = serde_json::from_slice(&std::fs::read(p)?)?;
    if r["schema_version"] != "nova.clients.v0.1" || !r["clients"].is_array() {
        return Err(NovaError::Validation("invalid registry".into()));
    }
    Ok(r)
}
pub(super) async fn clients(State(s): State<ReviewServer>) -> Response {
    match read_registry(&s) {Ok(r)=>Json(json!({"clients":r["clients"].as_array().unwrap().iter().filter(|e|e["principal"]["owner_id"]==s.owner&&e["principal"]["space_id"]==s.space).map(|e|json!({"principal":e["principal"],"expires_at":e["expires_at"],"revoked":e["revoked"],"connection":"not_verified"})).collect::<Vec<_>>(),"owner":s.owner,"space":s.space})).into_response(),Err(e)=>failure(e)}
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClientInput {
    client_id: String,
    operation: String,
    capabilities: Vec<String>,
    ttl_seconds: i64,
    acknowledge: bool,
}
pub(super) async fn client(State(s): State<ReviewServer>, Json(i): Json<ClientInput>) -> Response {
    if !i.acknowledge {
        return failure(NovaError::Validation(
            "owner acknowledgement required".into(),
        ));
    }
    let _lock = s.lock.lock().await;
    let result = (|| -> Result<serde_json::Value, NovaError> {
        let principal = nova_mcp::registry::Principal {
            owner_id: s.owner.clone(),
            space_id: s.space.clone(),
            client_id: i.client_id.clone(),
            capabilities: i.capabilities,
        };
        principal.validate()?;
        if i.client_id == "chatgpt-generic" {
            return Err(NovaError::Validation(
                "use chatgpt as the registered ID".into(),
            ));
        }
        let mut r = read_registry(&s)?;
        let rows = r["clients"].as_array_mut().unwrap();
        let response = if i.operation == "revoke" {
            let e = rows
                .iter_mut()
                .find(|e| {
                    e["principal"]["client_id"] == i.client_id
                        && e["principal"]["owner_id"] == s.owner
                        && e["principal"]["space_id"] == s.space
                })
                .ok_or_else(|| NovaError::PolicyDenied("client unavailable".into()))?;
            e["revoked"] = json!(true);
            json!({"status":"revoked"})
        } else if i.operation == "register" {
            if rows.len() >= 64
                || rows
                    .iter()
                    .any(|e| e["principal"]["client_id"] == i.client_id)
                || !(1..=31536000).contains(&i.ttl_seconds)
            {
                return Err(NovaError::Validation(
                    "duplicate client or invalid limits".into(),
                ));
            }
            let token = s
                .registry
                .as_ref()
                .unwrap()
                .parent()
                .unwrap()
                .join(format!("{}.client.key", i.client_id));
            VaultKey::generate()?.write_new_file(&token)?;
            // Clients never receive the owner credential or Vault key.
            rows.push(json!({"principal":principal,"token_file":token,"expires_at":(time_now()?+nova_time::Duration::seconds(i.ttl_seconds)).format(&nova_time::format_description::well_known::Rfc3339).map_err(|e|NovaError::Time(e.to_string()))?,"revoked":false}));
            json!({"status":"registered","token_file":token,"token":std::fs::read_to_string(&token)?.trim(),"client_id":i.client_id})
        } else {
            return Err(NovaError::Validation("unknown client operation".into()));
        };
        let p = s.registry.as_ref().unwrap();
        let mut temp = tempfile::NamedTempFile::new_in(p.parent().unwrap())?;
        temp.write_all(serde_json::to_string_pretty(&r)?.as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(p).map_err(|e| NovaError::Io(e.error))?;
        Ok(response)
    })();
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
fn time_now() -> Result<nova_time::OffsetDateTime, NovaError> {
    Ok(nova_time::OffsetDateTime::now_utc())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BackupInput {
    directory: String,
    #[serde(alias = "user_approved")]
    acknowledge: bool,
}
pub(super) async fn backup(State(s): State<ReviewServer>, Json(i): Json<BackupInput>) -> Response {
    if !i.acknowledge || !Path::new(&i.directory).is_absolute() {
        return failure(NovaError::Validation(
            "new absolute backup directory required".into(),
        ));
    }
    let _lock = s.lock.lock().await;
    let result = (|| -> Result<serde_json::Value, NovaError> {
        let dir = Path::new(&i.directory);
        std::fs::create_dir(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let package_key = VaultKey::generate()?;
        let key_path = dir.join("package.key");
        package_key.write_new_file(&key_path)?;
        let bytes = s.open()?.export_encrypted(&s.owner, &package_key)?;
        let package = dir.join("assets.nova");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&package)?;
        f.write_all(&bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(
            json!({"status":"exported_encrypted","scope":"legacy_vault_only","automatic_intake_included":false,"package":package,"package_key_file":key_path,"bytes":bytes.len(),"authority":"client credentials and disclosure grants excluded"}),
        )
    })();
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreInput {
    package: String,
    package_key_file: String,
    target_directory: String,
    #[serde(alias = "user_approved")]
    acknowledge: bool,
}
pub(super) async fn restore(
    State(s): State<ReviewServer>,
    Json(i): Json<RestoreInput>,
) -> Response {
    if !i.acknowledge
        || [&i.package, &i.package_key_file, &i.target_directory]
            .iter()
            .any(|v| !Path::new(v).is_absolute())
    {
        return failure(NovaError::Validation(
            "explicit absolute paths required".into(),
        ));
    }
    let _lock = s.lock.lock().await;
    let result = (|| -> Result<serde_json::Value, NovaError> {
        if std::fs::metadata(&i.package)?.len() > 64 * 1024 * 1024 {
            return Err(NovaError::Validation("package exceeds 64 MiB".into()));
        }
        let bytes = std::fs::read(&i.package)?;
        let package_key = VaultKey::read_file(Path::new(&i.package_key_file))?;
        nova_core::MigrationPackage::inspect_encrypted(&bytes, &package_key, &s.owner)?;
        let target = Path::new(&i.target_directory);
        std::fs::create_dir(target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o700))?;
        }
        let key = VaultKey::generate()?;
        let key_file = target.join("vault.key");
        key.write_new_file(&key_file)?;
        let path = target.join("vault.sqlite");
        let mut v = Vault::create(&path, key)?;
        v.preflight_import_encrypted(&bytes, &package_key, &s.owner)?;
        let report = v.import_encrypted(&bytes, &package_key, &s.owner)?;
        let audit = v.verify_audit_chain()?;
        Ok(
            json!({"status":"restored_to_new_target","vault":path,"key_file":key_file,"report":report,"audit":audit,"live_vault_switched":false,"client_access":"requires_new_owner_approval"}),
        )
    })();
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConnectionInput {
    client_id: String,
    address: String,
}
pub(super) async fn connection(
    State(s): State<ReviewServer>,
    Json(i): Json<ConnectionInput>,
) -> Response {
    let address = match i.address.parse::<std::net::SocketAddr>() {
        Ok(a) if a.ip().is_loopback() && a.port() != 0 => a,
        _ => return failure(NovaError::Validation("loopback address required".into())),
    };
    let credentials = (|| -> Result<String, NovaError> {
        let r = read_registry(&s)?;
        let entry = r["clients"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| {
                e["principal"]["client_id"] == i.client_id
                    && e["principal"]["owner_id"] == s.owner
                    && e["principal"]["space_id"] == s.space
            })
            .ok_or_else(|| NovaError::PolicyDenied("client unavailable".into()))?;
        nova_mcp::registry::RegistryCredential::load(
            s.registry.as_ref().unwrap(),
            &i.client_id,
            &s.key_path,
        )?;
        Ok(std::fs::read_to_string(
            entry["token_file"]
                .as_str()
                .ok_or_else(|| NovaError::Validation("invalid registry".into()))?,
        )?
        .trim()
        .into())
    })();
    let token = match credentials {
        Ok(t) => t,
        Err(e) => return failure(e),
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        use rmcp::ServiceExt;
        use rmcp::transport::{
            StreamableHttpClientTransport,
            streamable_http_client::StreamableHttpClientTransportConfig,
        };
        let client = rmcp::model::ClientInfo::default()
            .serve(StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp"))
                    .auth_header(token),
            ))
            .await
            .map_err(|_| ())?;
        let tools = client.list_all_tools().await.map_err(|_| ())?;
        client.cancel().await.map_err(|_| ())?;
        Ok::<bool, ()>(
            tools
                .iter()
                .map(|t| t.name.as_ref())
                .collect::<std::collections::HashSet<_>>()
                == ["save_memory", "get_memory", "search_memory"]
                    .into_iter()
                    .collect(),
        )
    })
    .await;
    Json(json!({"mcp_initialize":matches!(result,Ok(Ok(_))),"tool_discovery":matches!(result,Ok(Ok(true))),"authorized_business_call":"not_run","real_host":"not_verified"})).into_response()
}
