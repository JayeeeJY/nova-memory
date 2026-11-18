//! Bounded, read-only operational observations. Never opens source bodies or issues authority.
use super::{storage::*, *};
use rusqlite::Connection;
use std::collections::BTreeMap;
use time::OffsetDateTime;

pub const DOCTOR_ROWS_PER_LAYER: usize = 128;
pub const DOCTOR_ENCRYPTED_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DiagnosticLayer {
    pub observed: usize,
    pub omitted_unreadable: usize,
    pub complete: bool,
    pub counts: BTreeMap<String, usize>,
    pub last_success_at: Option<String>,
    pub oldest_pending_at: Option<String>,
    pub notices: Vec<String>,
}
impl DiagnosticLayer {
    fn increment(&mut self, name: &str) {
        *self.counts.entry(name.into()).or_default() += 1;
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct IntakeDiagnostics {
    pub schema_version: String,
    pub observed_at: String,
    pub model_location: String,
    pub layers: BTreeMap<String, DiagnosticLayer>,
    pub max_rows_per_layer: usize,
    pub max_encrypted_bytes: usize,
    pub encrypted_bytes_examined: usize,
    pub notices: Vec<String>,
}
#[derive(Deserialize)]
struct Grant {
    grant: CaptureGrant,
}
#[derive(Deserialize)]
struct ConnectionPolicy {
    policy: MemoryConnection,
}
#[derive(Deserialize)]
struct Ack {
    ack: CaptureAck,
}

fn later(target: &mut Option<String>, value: &str) -> Result<()> {
    let value_time = parse_time(value)?;
    if target
        .as_deref()
        .map(parse_time)
        .transpose()?
        .is_none_or(|t| value_time > t)
    {
        *target = Some(value.into());
    }
    Ok(())
}
fn earlier(target: &mut Option<String>, value: &str) -> Result<()> {
    let value_time = parse_time(value)?;
    if target
        .as_deref()
        .map(parse_time)
        .transpose()?
        .is_none_or(|t| value_time < t)
    {
        *target = Some(value.into());
    }
    Ok(())
}
fn rows(c: &Connection, kind: &str, budget: &mut usize) -> Result<(Vec<String>, DiagnosticLayer)> {
    let mut query = c.prepare("SELECT id,length(ciphertext) FROM intake_objects WHERE kind=?1 ORDER BY rowid DESC LIMIT ?2")?;
    let rows = query
        .query_map(rusqlite::params![kind, DOCTOR_ROWS_PER_LAYER + 1], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, usize>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut layer = DiagnosticLayer {
        complete: rows.len() <= DOCTOR_ROWS_PER_LAYER,
        ..Default::default()
    };
    if !layer.complete {
        layer.notices.push("latest_window_only".into());
    }
    let mut ids = Vec::new();
    for (id, size) in rows.into_iter().take(DOCTOR_ROWS_PER_LAYER) {
        if !super::consolidation::readable(c, &id)? {
            layer.omitted_unreadable += 1;
            continue;
        }
        if size > *budget {
            layer.complete = false;
            layer.notices.push("encrypted_byte_budget".into());
            break;
        }
        *budget -= size;
        let prefix = physical_id(kind, "");
        ids.push(
            id.strip_prefix(&prefix)
                .ok_or_else(|| invalid("diagnostic_record_kind"))?
                .to_owned(),
        );
    }
    layer.observed = ids.len();
    Ok((ids, layer))
}

impl IntakeStore {
    /// Scope is an authenticated Owner binding. Counters are bounded observations, not totals.
    /// Opening the report does not probe any model, install Hook, recover a lease or call MCP.
    pub fn operational_diagnostics(&self, scope: &AssetScope) -> Result<IntakeDiagnostics> {
        self.check(scope)?;
        let tx = self.connection.unchecked_transaction()?;
        let current = OffsetDateTime::now_utc();
        let mut budget = DOCTOR_ENCRYPTED_BYTES;
        let mut layers = BTreeMap::new();
        let (ids, mut jobs) = rows(&tx, "job", &mut budget)?;
        for id in ids {
            let job: IntakeJob = load(&tx, &self.key, "job", &id)?;
            if job.job.scope != *scope {
                return Err(denied());
            }
            match job.job.state {
                JobState::Received => {
                    jobs.increment("waiting");
                    earlier(&mut jobs.oldest_pending_at, &job.receipt.saved_at)?;
                }
                JobState::Processing => {
                    let live = job
                        .lease
                        .as_ref()
                        .map(|l| parse_time(&l.expires_at))
                        .transpose()?
                        .is_some_and(|t| t > current);
                    jobs.increment(if live {
                        "processing_lease_observed"
                    } else {
                        "expired_lease"
                    });
                    earlier(&mut jobs.oldest_pending_at, &job.receipt.saved_at)?;
                }
                JobState::Ready => {
                    jobs.increment("ready");
                    later(&mut jobs.last_success_at, &job.updated_at)?;
                }
                JobState::Failed => {
                    jobs.increment("failed");
                    let code = job.history.last().map(|h| h.reason.as_str()).unwrap_or("");
                    jobs.increment(match code {
                        "model_unavailable" => "model_unavailable",
                        "invalid_output" => "invalid_output",
                        "budget_exceeded" => "budget_exceeded",
                        "worker_interrupted" => "worker_interrupted",
                        "configuration_changed" => "configuration_changed",
                        _ => "other_failure",
                    });
                }
            }
        }
        jobs.notices
            .push("ready_is_committed_output_not_semantic_quality".into());
        jobs.notices.push("lease_is_not_worker_heartbeat".into());
        layers.insert("compiler_queue".into(), jobs);

        let (ids, mut grants) = rows(&tx, "capture_grant", &mut budget)?;
        for id in ids {
            let g: Grant = load(&tx, &self.key, "capture_grant", &id)?;
            let g = g.grant;
            if g.scope != *scope {
                return Err(denied());
            }
            grants.increment(if g.revoked {
                "revoked"
            } else if parse_time(&g.expires_at)? <= current {
                "expired"
            } else if g.received_events >= g.event_budget || g.received_bytes >= g.byte_budget {
                "exhausted"
            } else {
                "active"
            });
        }
        grants
            .notices
            .push("grant_is_not_hook_installation_or_host_acceptance".into());
        layers.insert("capture_grants".into(), grants);

        let (ids, mut captures) = rows(&tx, "capture_event", &mut budget)?;
        for id in ids {
            let ack: Ack = load(&tx, &self.key, "capture_event", &id)?;
            later(&mut captures.last_success_at, &ack.ack.receipt.saved_at)?;
            captures.increment("source_acked");
            if ack.ack.loss.truncated {
                captures.increment("truncated");
            }
            if ack.ack.loss.redacted {
                captures.increment("redacted");
            }
            if ack.ack.loss.partial_history {
                captures.increment("partial_history");
            }
        }
        captures
            .notices
            .push("acked_source_not_compiled_memory_or_complete_history".into());
        layers.insert("capture_receipts".into(), captures);

        let (ids, mut connections) = rows(&tx, "connection", &mut budget)?;
        for id in ids {
            let p: ConnectionPolicy = load(&tx, &self.key, "connection", &id)?;
            let p = p.policy;
            if p.scope != *scope {
                return Err(denied());
            }
            connections.increment(if p.revoked {
                "revoked"
            } else if parse_time(&p.expires_at)? <= current {
                "expired"
            } else if p.used_calls >= p.call_budget {
                "exhausted"
            } else {
                "active"
            });
        }
        layers.insert("connections".into(), connections);
        let (ids, mut usage) = rows(&tx, "connection_usage", &mut budget)?;
        for id in ids {
            let u: ConnectionUsage = load(&tx, &self.key, "connection_usage", &id)?;
            match u.operation.as_str() {
                "read" | "read_historical" => {
                    usage.increment("read_receipts");
                    later(&mut usage.last_success_at, &u.at)?;
                    if u.item_count == 0 {
                        usage.increment("empty_read");
                    }
                }
                "capture" => usage.increment("capture_receipts"),
                _ => usage.increment("other_receipts"),
            }
        }
        usage
            .notices
            .push("server_recorded_only_not_host_task_acceptance".into());
        layers.insert("disclosure".into(), usage);
        let config: ModelConfiguration = load(&tx, &self.key, "configuration", "configuration")?;
        config.validate()?;
        let location = match config.selection {
            ModelSelection::Unconfigured => "unconfigured",
            ModelSelection::Local { .. } => "local",
            ModelSelection::Remote { .. } => "remote",
        };
        tx.commit()?;
        Ok(IntakeDiagnostics {
            schema_version: "nova.operational-diagnostics.v0.1".into(),
            observed_at: format_time(current)?,
            model_location: location.into(),
            layers,
            max_rows_per_layer: DOCTOR_ROWS_PER_LAYER,
            max_encrypted_bytes: DOCTOR_ENCRYPTED_BYTES,
            encrypted_bytes_examined: DOCTOR_ENCRYPTED_BYTES - budget,
            notices: vec![
                "bounded_metadata_snapshot_no_source_text".into(),
                "not_a_model_probe_or_integrity_audit".into(),
                "relay_and_hook_runtime_not_observed_by_store".into(),
            ],
        })
    }
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
