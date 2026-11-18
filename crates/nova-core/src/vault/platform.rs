//! Restricted, encrypted, versioned capture presets. They never alter canonical truth.
use super::*;
impl Vault {
    pub fn owner_templates(&self, owner: &str) -> Result<Vec<StructureTemplate>> {
        self.verify_audit_chain()?;
        let mut q = self.connection.prepare("SELECT DISTINCT aggregate_id FROM event_log WHERE event_type='StructureTemplateUpdated'")?;
        let ids = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = vec![];
        for id in ids {
            let t: StructureTemplate =
                load_event_payload(&self.connection, &self.key, "StructureTemplateUpdated", &id)?;
            if t.owner_id == owner {
                out.push(t);
            }
        }
        Ok(out)
    }
    pub fn owner_template_versions(&self, owner: &str, id: &str) -> Result<Vec<StructureTemplate>> {
        self.verify_audit_chain()?;
        let versions = template_history(&self.connection, &self.key, Some(id))?;
        if versions.is_empty() || versions.iter().any(|t| t.owner_id != owner) {
            return Err(NovaError::PolicyDenied("template unavailable".into()));
        }
        Ok(versions)
    }
    pub fn owner_template_version(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
    ) -> Result<StructureTemplate> {
        self.owner_template_versions(owner, id)?
            .into_iter()
            .find(|t| t.revision == revision)
            .ok_or_else(|| NovaError::Validation("template version unavailable".into()))
    }
    pub fn put_template(
        &mut self,
        owner: &str,
        mut input: StructureTemplate,
        expected_revision: Option<u64>,
    ) -> Result<StructureTemplate> {
        input.validate_definition()?;
        self.verify_audit_chain()?;
        if owner.is_empty() {
            return Err(NovaError::Validation("invalid template".into()));
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (version_count, template_count): (u64, u64) = tx.query_row(
            "SELECT COUNT(*), COUNT(DISTINCT aggregate_id) FROM event_log WHERE event_type='StructureTemplateUpdated'", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if version_count >= 10_000 || (input.template_id.is_empty() && template_count >= 100) {
            return Err(NovaError::Validation("template storage limit".into()));
        }
        let (kid, key) = if input.template_id.is_empty() {
            if expected_revision.is_some() {
                return Err(NovaError::InvalidState("unexpected revision".into()));
            }
            input.template_id = new_id("template");
            input.revision = 1;
            create_data_key(&tx, &self.key, &input.template_id, "structure_template")?
        } else {
            let old: StructureTemplate = load_event_payload(
                &tx,
                &self.key,
                "StructureTemplateUpdated",
                &input.template_id,
            )?;
            if old.owner_id != owner || expected_revision != Some(old.revision) {
                return Err(NovaError::InvalidState("template revision changed".into()));
            }
            let history = template_history(&tx, &self.key, Some(&input.template_id))?;
            if history.len() >= 1000 {
                return Err(NovaError::Validation("template version limit".into()));
            }
            for field in &input.fields {
                if history
                    .iter()
                    .flat_map(|t| &t.fields)
                    .any(|previous| previous.id == field.id && previous.kind != field.kind)
                {
                    return Err(NovaError::Validation(
                        "field kind changed; create a new field ID".into(),
                    ));
                }
            }
            input.revision = old
                .revision
                .checked_add(1)
                .ok_or_else(|| NovaError::Validation("template revision overflow".into()))?;
            load_key_by_aggregate(&tx, &self.key, &input.template_id)?
        };
        input.owner_id = owner.into();
        append_event(
            &tx,
            &self.audit_key,
            &kid,
            &key,
            "StructureTemplateUpdated",
            &input.template_id,
            &input,
        )?;
        tx.commit()?;
        Ok(input)
    }
}

/// Export every published definition, including revisions used by old records.
pub(super) fn template_history(
    connection: &Connection,
    key: &VaultKey,
    id: Option<&str>,
) -> Result<Vec<StructureTemplate>> {
    let mut statement = connection.prepare("SELECT event_id, aggregate_id, key_id, nonce, ciphertext FROM event_log WHERE event_type='StructureTemplateUpdated' AND (?1 IS NULL OR aggregate_id=?1) ORDER BY sequence ASC")?;
    let rows = statement
        .query_map(params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, Vec<u8>>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(event, aggregate, kid, nonce, ciphertext)| {
            let k = load_key_by_id(connection, key, &kid)?;
            let aad = format!("event:{event}:StructureTemplateUpdated:{aggregate}:v1");
            let data = k.decrypt(&EncryptedBlob { nonce, ciphertext }, aad.as_bytes())?;
            Ok(serde_json::from_slice(&data)?)
        })
        .collect()
}
