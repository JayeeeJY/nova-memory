//! User-selected normalized visible JSONL only. No path discovery or proprietary history parsing.
use super::*;
pub const CAPTURE_HISTORY_VERSION: &str = "nova.capture.visible-jsonl.v0.1";
pub const MAX_CAPTURE_HISTORY_BYTES: usize = 8 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureVisibleHistoryLine {
    pub schema_version: String,
    pub host: String,
    pub origin_session_id: String,
    pub host_turn_id: String,
    pub kind: CaptureEventKind,
    pub content: String,
    pub occurred_at: Option<String>,
    pub loss: CaptureLoss,
    pub origin: CaptureDeliveryOrigin,
}
impl CaptureVisibleHistoryLine {
    /// Uses exactly the live adapter's Host/session/turn/kind derivation. Sequence is assigned once
    /// by the shared bootstrap ledger; file order never rewrites an existing ingress sequence.
    pub fn project(&self) -> Result<CaptureRelayEvent> {
        if self.schema_version != CAPTURE_HISTORY_VERSION
            || self.origin != CaptureDeliveryOrigin::UserProvidedExport
            || !self.loss.partial_history
        {
            return Err(invalid("history_export_contract"));
        }
        let host = match self.host.as_str() {
            "codex" => CaptureHookHost::CodexCli,
            "claude" => CaptureHookHost::ClaudeCodeCli,
            _ => return Err(invalid("history_host_unsupported")),
        };
        let name = match self.kind {
            CaptureEventKind::UserPrompt => "UserPromptSubmit",
            CaptureEventKind::AssistantResponse => "Stop",
        };
        let mut value = serde_json::json!({"session_id":self.origin_session_id,"turn_id":self.host_turn_id,"prompt_id":self.host_turn_id,"hook_event_name":name,"stop_hook_active":false});
        value[if self.kind == CaptureEventKind::UserPrompt {
            "prompt"
        } else {
            "last_assistant_message"
        }] = serde_json::json!(self.content);
        let mut event = decode_visible_capture_hook(
            host,
            &self.origin_session_id,
            1,
            &serde_json::to_vec(&value)?,
        )?;
        event.loss = self.loss.clone();
        event.occurred_at = self.occurred_at.clone();
        if let Some(t) = &event.occurred_at {
            if t.len() > 64 {
                return Err(invalid("history_time_bounds"));
            }
            parse_time(t)?;
        }
        Ok(event)
    }
}
pub fn parse_visible_history_jsonl(
    bytes: &[u8],
    expected_host: &str,
) -> Result<Vec<CaptureVisibleHistoryLine>> {
    if bytes.is_empty() || bytes.len() > MAX_CAPTURE_HISTORY_BYTES {
        return Err(invalid("history_file_bounds"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("history_utf8_required"))?;
    let mut lines = vec![];
    let mut identities = std::collections::BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() || line.len() > 1024 * 1024 || lines.len() >= 1000 {
            return Err(invalid("history_line_bounds"));
        }
        let value: CaptureVisibleHistoryLine =
            crate::compilation::decode_untrusted_json(line.as_bytes())?;
        if value.host != expected_host {
            return Err(invalid("history_binding_host_mismatch"));
        }
        let event = value.project()?;
        let key = (value.origin_session_id.clone(), event.event_id.clone());
        let serialized = serde_json::to_vec(&event)?;
        if identities.get(&key).is_some_and(|old| old != &serialized) {
            return Err(conflict());
        }
        identities.insert(key, serialized);
        lines.push(value);
    }
    if lines.is_empty() {
        return Err(invalid("history_file_bounds"));
    }
    Ok(lines)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureHistoryFile {
    pub import_id: String,
    pub file_sha256: String,
    pub file_bytes: usize,
    pub line_count: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureHistoryPhase {
    Ready,
    PendingSource,
    PendingProvenance,
    Complete,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureHistoryProgress {
    pub schema_version: String,
    pub file: CaptureHistoryFile,
    pub next_line: usize,
    pub phase: CaptureHistoryPhase,
    pub origin: CaptureDeliveryOrigin,
    pub host_verified: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureHistoryLineReceipt {
    pub line: usize,
    pub receipt: IntakeReceipt,
    pub provenance: CaptureProvenanceRecord,
}
#[derive(Serialize, Deserialize)]
struct JournalIdentity {
    version: String,
    configuration: CaptureBootstrapConfiguration,
    binding: String,
}
pub struct CaptureHistoryJournal {
    connection: Connection,
    key: VaultKey,
    configuration: CaptureBootstrapConfiguration,
    binding: String,
}
fn valid_file(file: &CaptureHistoryFile) -> Result<()> {
    if !identifier(&file.import_id)
        || file.file_sha256.len() != 64
        || !file.file_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !(1..=MAX_CAPTURE_HISTORY_BYTES).contains(&file.file_bytes)
        || !(1..=1000).contains(&file.line_count)
    {
        return Err(invalid("history_checkpoint_file"));
    }
    Ok(())
}
impl CaptureHistoryJournal {
    fn import_row(&self, import_id: &str) -> String {
        format!(
            "import:{}",
            self.key
                .authentication_tag(&[b"capture-history-file:v1", import_id.as_bytes()])
        )
    }
    fn line_row(&self, import_id: &str, line: usize) -> String {
        format!(
            "line:{}",
            self.key.authentication_tag(&[
                b"capture-history-line:v1",
                import_id.as_bytes(),
                &(line as u64).to_be_bytes()
            ])
        )
    }

    pub fn create(
        path: &Path,
        key: VaultKey,
        configuration: CaptureBootstrapConfiguration,
    ) -> Result<Self> {
        configuration.validate()?;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        o.open(path)?.sync_all()?;
        let connection = Connection::open(path)?;
        configure_relay(&connection, 16384)?;
        connection.execute_batch("CREATE TABLE relay_objects(id TEXT PRIMARY KEY,nonce BLOB NOT NULL,ciphertext BLOB NOT NULL);")?;
        let binding = key.authentication_tag(&[
            b"capture-history-journal:v1",
            &serde_json::to_vec(&configuration)?,
        ]);
        seal(
            &connection,
            &key,
            "",
            "identity",
            &JournalIdentity {
                version: CAPTURE_HISTORY_VERSION.into(),
                configuration: configuration.clone(),
                binding: binding.clone(),
            },
        )?;
        Ok(Self {
            connection,
            key,
            configuration,
            binding,
        })
    }
    pub fn open(
        path: &Path,
        key: VaultKey,
        expected: CaptureBootstrapConfiguration,
    ) -> Result<Self> {
        expected.validate()?;
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        let i: JournalIdentity = unseal(&connection, &key, "", "identity")?;
        let binding = key.authentication_tag(&[
            b"capture-history-journal:v1",
            &serde_json::to_vec(&expected)?,
        ]);
        if i.version != CAPTURE_HISTORY_VERSION
            || i.configuration != expected
            || i.binding != binding
        {
            return Err(denied());
        }
        configure_relay(&connection, 16384)?;
        Ok(Self {
            connection,
            key,
            configuration: expected,
            binding,
        })
    }
    pub fn begin(&mut self, file: CaptureHistoryFile) -> Result<CaptureHistoryProgress> {
        valid_file(&file)?;
        let id = self.import_row(&file.import_id);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM relay_objects WHERE id=?1)",
            [&id],
            |r| r.get(0),
        )?;
        if exists {
            let p: CaptureHistoryProgress = unseal(&tx, &self.key, &self.binding, &id)?;
            if p.file != file {
                return Err(conflict());
            }
            return Ok(p);
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM relay_objects WHERE id LIKE 'import:%'",
            [],
            |r| r.get(0),
        )?;
        if count >= 16 {
            return Err(invalid("history_import_capacity"));
        }
        let p = CaptureHistoryProgress {
            schema_version: CAPTURE_HISTORY_VERSION.into(),
            file,
            next_line: 0,
            phase: CaptureHistoryPhase::Ready,
            origin: CaptureDeliveryOrigin::UserProvidedExport,
            host_verified: false,
        };
        seal(&tx, &self.key, &self.binding, &id, &p)?;
        tx.commit()?;
        Ok(p)
    }
    pub fn pending(
        &mut self,
        file: &CaptureHistoryFile,
        line: usize,
        phase: CaptureHistoryPhase,
    ) -> Result<CaptureHistoryProgress> {
        if !matches!(
            phase,
            CaptureHistoryPhase::PendingSource | CaptureHistoryPhase::PendingProvenance
        ) {
            return Err(invalid("history_pending_phase"));
        }
        let id = self.import_row(&file.import_id);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut p: CaptureHistoryProgress = unseal(&tx, &self.key, &self.binding, &id)?;
        if p.file != *file || line != p.next_line || line >= file.line_count {
            return Err(conflict());
        }
        p.phase = phase;
        seal(&tx, &self.key, &self.binding, &id, &p)?;
        tx.commit()?;
        Ok(p)
    }
    pub fn confirm(
        &mut self,
        file: &CaptureHistoryFile,
        line: usize,
        expected: &CaptureVisibleHistoryLine,
        receipt: IntakeReceipt,
        provenance: CaptureProvenanceRecord,
    ) -> Result<CaptureHistoryProgress> {
        let mut event = expected.project()?;
        event.sequence = provenance.sequence;
        let event = capture_bootstrap::prepared(event, &self.configuration.policy)?;
        if provenance.schema_version != CAPTURE_PROVENANCE_VERSION
            || provenance.event_id != event.event_id
            || provenance.kind != event.kind
            || provenance.loss != event.loss
            || provenance.source_session.origin_session_id != expected.origin_session_id
            || receipt.content_sha256 != hex::encode(Sha256::digest(event.content.as_bytes()))
            || provenance.instance_id != self.configuration.instance_id
            || provenance.source_session.source_namespace != self.configuration.source_namespace
            || provenance.source != receipt.source
            || provenance.event_key != receipt.request_id
            || !provenance
                .origins
                .contains(&CaptureDeliveryOrigin::UserProvidedExport)
            || !provenance.loss.partial_history
            || receipt.schema_version != INTAKE_VERSION
            || receipt.status != IntakeAcceptance::Received
        {
            return Err(denied());
        }
        let id = self.import_row(&file.import_id);
        let line_row = self.line_row(&file.import_id, line);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut p: CaptureHistoryProgress = unseal(&tx, &self.key, &self.binding, &id)?;
        if p.file != *file || line != p.next_line || line >= file.line_count {
            return Err(conflict());
        }
        seal(
            &tx,
            &self.key,
            &self.binding,
            &line_row,
            &CaptureHistoryLineReceipt {
                line,
                receipt,
                provenance,
            },
        )?;
        p.next_line += 1;
        p.phase = if p.next_line == p.file.line_count {
            CaptureHistoryPhase::Complete
        } else {
            CaptureHistoryPhase::Ready
        };
        seal(&tx, &self.key, &self.binding, &id, &p)?;
        tx.commit()?;
        Ok(p)
    }
    pub fn confirmed_receipts(
        &self,
        file: &CaptureHistoryFile,
    ) -> Result<Vec<CaptureHistoryLineReceipt>> {
        let p: CaptureHistoryProgress = unseal(
            &self.connection,
            &self.key,
            &self.binding,
            &self.import_row(&file.import_id),
        )?;
        if p.file != *file || p.next_line > 1000 {
            return Err(denied());
        }
        (0..p.next_line)
            .map(|line| {
                let line_row = self.line_row(&file.import_id, line);
                unseal(&self.connection, &self.key, &self.binding, &line_row)
            })
            .collect()
    }
}
