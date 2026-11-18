//! Delivery descriptions are additive metadata, not canonical content, verified identity or grants.
use super::{compiler::*, storage::*, *};
use rusqlite::{TransactionBehavior, params};
pub const CAPTURE_PROVENANCE_VERSION: &str = "nova.capture.provenance.v0.1";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureDeliveryOrigin {
    HookUnverified,
    UserProvidedExport,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureProvenanceRequest {
    pub session_id: String,
    pub event_id: String,
    pub kind: CaptureEventKind,
    pub origin: CaptureDeliveryOrigin,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureProvenanceRecord {
    pub schema_version: String,
    pub scope: AssetScope,
    pub instance_id: String,
    pub event_key: String,
    pub source: RevisionRef,
    pub source_session: CaptureSession,
    pub event_id: String,
    pub sequence: u64,
    pub kind: CaptureEventKind,
    pub loss: CaptureLoss,
    pub origins: Vec<CaptureDeliveryOrigin>,
    pub interpretation: String,
}
/// Bounded Owner inventory. A cursor advances across withdrawn records too; this is not a
/// complete transcript or a count of all events in the originating Host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureProvenancePage {
    pub items: Vec<CaptureProvenanceRecord>,
    pub next_before: Option<i64>,
    pub scanned_records: usize,
    pub full_history: bool,
}
impl IntakeStore {
    pub fn list_capture_provenance_for_owner(
        &self,
        scope: &AssetScope,
        before: Option<i64>,
    ) -> Result<CaptureProvenancePage> {
        self.check(scope)?;
        if before.is_some_and(|n| n <= 0) {
            return Err(invalid("capture_provenance_cursor"));
        }
        let rows = self
            .connection
            .prepare(
                "SELECT rowid,substr(id,length('n2a:capture_provenance:')+1) FROM intake_objects
             WHERE kind='capture_provenance' AND rowid<?1 ORDER BY rowid DESC LIMIT 21",
            )?
            .query_map([before.unwrap_or(i64::MAX)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let next_before = (rows.len() > 20).then(|| rows[19].0);
        let scanned_records = rows.len().min(20);
        let mut items = vec![];
        for (_, id) in rows.into_iter().take(20) {
            if !consolidation::readable(&self.connection, &physical_id("capture_provenance", &id))?
            {
                continue;
            }
            let record: CaptureProvenanceRecord =
                load(&self.connection, &self.key, "capture_provenance", &id)?;
            if record.scope != *scope || record.event_key != id || record.origins.len() > 2 {
                return Err(denied());
            }
            if load_source(&self.connection, &self.key, scope, &record.source).is_ok() {
                items.push(record);
            }
        }
        Ok(CaptureProvenancePage {
            items,
            next_before,
            scanned_records,
            full_history: false,
        })
    }
    /// Capture token may append only two bounded descriptions to its own already-received event.
    /// Canonical envelope/fingerprint/Source/role/fidelity remain unchanged, including on duplicate.
    pub fn record_capture_provenance(
        &mut self,
        scope: &AssetScope,
        access: &CaptureAccess,
        input: CaptureProvenanceRequest,
    ) -> Result<CaptureProvenanceRecord> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (grant, session, ack) = capture::received_for_provenance(
            &tx,
            &self.key,
            scope,
            access,
            &input.session_id,
            &input.event_id,
            input.kind,
        )?;
        let mut record = if exists(&tx, "capture_provenance", &ack.event_key)? {
            let record: CaptureProvenanceRecord =
                load(&tx, &self.key, "capture_provenance", &ack.event_key)?;
            if record.scope != *scope
                || record.instance_id != grant.instance_id
                || record.source != ack.receipt.source
                || record.event_key != ack.event_key
                || record.source_session.session_id != session.session_id
                || record.event_id != ack.event_id
                || record.kind != ack.kind
                || record.origins.len() > 2
            {
                return Err(denied());
            }
            record
        } else {
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM intake_objects WHERE kind='capture_provenance'",
                [],
                |r| r.get(0),
            )?;
            if count >= 10_000 {
                return Err(invalid("capture_provenance_capacity"));
            }
            CaptureProvenanceRecord{schema_version:CAPTURE_PROVENANCE_VERSION.into(),scope:scope.clone(),instance_id:grant.instance_id,event_key:ack.event_key.clone(),source:ack.receipt.source,source_session:session,event_id:ack.event_id,sequence:ack.sequence,kind:ack.kind,loss:ack.loss,origins:vec![],interpretation:"delivery_description_unverified; does_not_change_source_role_fidelity_or_authority; partial_history".into()}
        };
        if !record.origins.contains(&input.origin) {
            record.origins.push(input.origin);
        }
        if exists(&tx, "capture_provenance", &record.event_key)? {
            update(
                &tx,
                &self.key,
                "capture_provenance",
                &record.event_key,
                &record,
            )?;
        } else {
            insert(
                &tx,
                &self.key,
                "capture_provenance",
                &record.event_key,
                &record,
            )?;
        }
        tx.commit()?;
        Ok(record)
    }
    /// Owner-only inventory. No capture/reader token HTTP GET surface exposes this method.
    pub fn capture_provenance_for_owner(
        &self,
        scope: &AssetScope,
        event_key: &str,
    ) -> Result<CaptureProvenanceRecord> {
        self.check(scope)?;
        if !identifier(event_key) {
            return Err(denied());
        }
        let record: CaptureProvenanceRecord =
            load(&self.connection, &self.key, "capture_provenance", event_key)?;
        if record.scope != *scope {
            return Err(denied());
        }
        load_source(&self.connection, &self.key, scope, &record.source)?;
        Ok(record)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureContinuityLinkRequest {
    pub event_key: String,
    pub recipient_session: SessionRef,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureContinuityAssociation {
    pub schema_version: String,
    pub association_id: String,
    pub scope: AssetScope,
    pub source: RevisionRef,
    pub source_session: CaptureSession,
    pub recipient_session: SessionRef,
    pub event_key: String,
    pub delivery_origins: Vec<CaptureDeliveryOrigin>,
    pub origin: String,
    pub confers_read_authority: bool,
    pub recap_completed: bool,
}
impl IntakeStore {
    /// Explicit owner metadata link only. Does not register a continuity event, charge/read a grant,
    /// change the job actor, deliver a Handoff, or create an executable/context-bearing object.
    pub fn link_capture_source_for_owner(
        &mut self,
        scope: &AssetScope,
        input: CaptureContinuityLinkRequest,
    ) -> Result<CaptureContinuityAssociation> {
        self.check(scope)?;
        if !identifier(&input.event_key) {
            return Err(denied());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        continuity::validate_owner_session(&tx, &self.key, scope, &input.recipient_session)?;
        let provenance: CaptureProvenanceRecord =
            load(&tx, &self.key, "capture_provenance", &input.event_key)?;
        if provenance.scope != *scope
            || provenance.source_session.session_id == input.recipient_session.session_id
        {
            return Err(denied());
        }
        load_source(&tx, &self.key, scope, &provenance.source)?;
        let id = self.key.authentication_tag(&[
            b"capture-continuity-association:v1",
            &serde_json::to_vec(&(&input.event_key, &input.recipient_session))?,
        ]);
        if exists(&tx, "capture_continuity_link", &id)? {
            let old = load(&tx, &self.key, "capture_continuity_link", &id)?;
            return Ok(old);
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM intake_objects WHERE kind='capture_continuity_link'",
            [],
            |r| r.get(0),
        )?;
        if count >= 256 {
            return Err(invalid("capture_continuity_link_capacity"));
        }
        let association = CaptureContinuityAssociation {
            schema_version: "nova.capture.continuity-link.v0.1".into(),
            association_id: id.clone(),
            scope: scope.clone(),
            source: provenance.source,
            source_session: provenance.source_session,
            recipient_session: input.recipient_session,
            event_key: input.event_key,
            delivery_origins: provenance.origins,
            origin: "owner_linked_unverified".into(),
            confers_read_authority: false,
            recap_completed: false,
        };
        insert(&tx, &self.key, "capture_continuity_link", &id, &association)?;
        tx.commit()?;
        Ok(association)
    }
    pub fn capture_continuity_associations_for_owner(
        &self,
        scope: &AssetScope,
        recipient: &SessionRef,
    ) -> Result<Vec<CaptureContinuityAssociation>> {
        self.check(scope)?;
        continuity::validate_owner_session(&self.connection, &self.key, scope, recipient)?;
        let ids=self.connection.prepare("SELECT substr(id,length('n2a:capture_continuity_link:')+1) FROM intake_objects WHERE kind='capture_continuity_link' ORDER BY rowid LIMIT 257")?.query_map(params![],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        if ids.len() > 256 {
            return Err(invalid("capture_continuity_link_capacity"));
        }
        let mut result = vec![];
        for id in ids {
            if !consolidation::readable(
                &self.connection,
                &physical_id("capture_continuity_link", &id),
            )? {
                continue;
            }
            let a: CaptureContinuityAssociation =
                load(&self.connection, &self.key, "capture_continuity_link", &id)?;
            if a.scope != *scope {
                return Err(denied());
            }
            if a.recipient_session == *recipient
                && load_source(&self.connection, &self.key, scope, &a.source).is_ok()
            {
                result.push(a);
            }
        }
        Ok(result)
    }
    /// Remove only this Owner metadata relation, even when its source/session is no longer live.
    /// A later explicit link can recreate it; this does not restore any source or read authority.
    pub fn remove_capture_continuity_association_for_owner(
        &mut self,
        scope: &AssetScope,
        association_id: &str,
    ) -> Result<bool> {
        self.check(scope)?;
        if !identifier(association_id) {
            return Err(denied());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !exists(&tx, "capture_continuity_link", association_id)? {
            return Ok(false);
        }
        let physical = physical_id("capture_continuity_link", association_id);
        if consolidation::readable(&tx, &physical)? {
            let record: CaptureContinuityAssociation =
                load(&tx, &self.key, "capture_continuity_link", association_id)?;
            if record.scope != *scope || record.association_id != association_id {
                return Err(denied());
            }
        }
        // A withdrawn dependency must not be decrypted to allow its Owner to erase metadata.
        // This store's verified identity already pins the scope; no record contents are returned.
        if consolidation::available(&tx)? {
            consolidation::remove_record(&tx, "capture_continuity_link", association_id)?;
        } else {
            tx.execute("DELETE FROM intake_objects WHERE id=?1 AND kind='capture_continuity_link'", [physical])?;
        }
        tx.commit()?;
        Ok(true)
    }
}
