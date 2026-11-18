//! Selected immutable visible JSONL snapshot -> shared relay -> Source/provenance double receipt.
use super::*;

#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureHistoryImportReport {
    pub progress: CaptureHistoryProgress,
    pub completed: bool,
    pub processed_this_run: usize,
    pub received_sources: Vec<CaptureHistoryLineReceipt>,
    pub interpretation: String,
}
fn history_file(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    // Explicitly selected regular owner file; never follows final symlink or discovers other paths.
    absolute(path)?;
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = o.open(path)?;
    let before = f.metadata()?;
    ensure!(
        before.is_file() && before.len() <= MAX_CAPTURE_HISTORY_BYTES as u64,
        "history_selected_file_bounds"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            before.uid() == unsafe { libc::geteuid() }
                && before.nlink() == 1
                && before.mode() & 0o022 == 0,
            "history_selected_file_ownership"
        );
    }
    let mut bytes = Zeroizing::new(Vec::new());
    (&f).take(MAX_CAPTURE_HISTORY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let after = f.metadata()?;
    ensure!(
        bytes.len() <= MAX_CAPTURE_HISTORY_BYTES
            && bytes.len() as u64 == before.len()
            && before.len() == after.len()
            && before.modified()? == after.modified()?,
        "history_file_changed_during_snapshot"
    );
    Ok(bytes)
}
fn journal(binding: &Binding, root: &Path) -> Result<CaptureHistoryJournal> {
    let p = root.join("history.sqlite");
    let k = key(&binding.key_file)?;
    let cfg = bootstrap_configuration(binding);
    if p.try_exists()? {
        private_open(&p)?;
        Ok(CaptureHistoryJournal::open(&p, k, cfg)?)
    } else {
        Ok(CaptureHistoryJournal::create(&p, k, cfg)?)
    }
}
/// max_events bounds this explicit run; pending work is resumed only by an explicit later invocation.
/// Whole-file preflight precedes all event writes. Same selected path must keep the exact fingerprint.
pub fn import_visible_capture_history(
    binding_file: &Path,
    file: &Path,
    max_events: usize,
) -> Result<CaptureHistoryImportReport> {
    ensure!((1..=16).contains(&max_events), "history_run_budget");
    let bytes = history_file(file)?;
    let canonical = file.canonicalize()?;
    let (binding, root, lock) = load(binding_file)?;
    let lines = parse_visible_history_jsonl(&bytes, &binding.host)?;
    let identity = CaptureHistoryFile {
        import_id: format!(
            "history-{}",
            hex_digest(canonical.as_os_str().as_encoded_bytes())
        ),
        file_sha256: hex_digest(&bytes),
        file_bytes: bytes.len(),
        line_count: lines.len(),
    };
    let history_lock_path = root.join("history.lock");
    if !history_lock_path.try_exists()? {
        write_new(&history_lock_path, b"")?;
    }
    let history_lock = private_open(&history_lock_path)?;
    history_lock.try_lock()?;
    let mut journal = journal(&binding, &root)?;
    let mut progress = journal.begin(identity.clone())?;
    drop(lock); // Release consumer serialization between bounded per-line deliveries; keep journal lock.
    let mut processed = 0;
    while progress.next_line < lines.len() && processed < max_events {
        let index = progress.next_line;
        let line = &lines[index];
        let event = line.project()?;
        let (current, current_root, _lock) = load(binding_file)?;
        ensure!(
            serde_json::to_vec(&current)? == serde_json::to_vec(&binding)?,
            "history_binding_changed"
        );
        let outcome = process_visible_event(
            &current,
            &current_root,
            &line.origin_session_id,
            event,
            CaptureDeliveryOrigin::UserProvidedExport,
        )?;
        processed += 1;
        match (outcome.receipt, outcome.provenance) {
            (Some(receipt), Some(provenance)) => {
                progress = journal.confirm(&identity, index, line, receipt, provenance)?
            }
            (Some(_), None) => {
                progress =
                    journal.pending(&identity, index, CaptureHistoryPhase::PendingProvenance)?;
                break;
            }
            _ => {
                progress = journal.pending(&identity, index, CaptureHistoryPhase::PendingSource)?;
                break;
            }
        }
    }
    let completed = progress.phase == CaptureHistoryPhase::Complete;
    let received_sources = journal.confirmed_receipts(&identity)?;
    Ok(CaptureHistoryImportReport{progress,completed,processed_this_run:processed,received_sources,interpretation:"user_provided_export; partial_history; local_checkpoint_and_Source_plus_provenance_receipts; Host_not_verified; no_read_grant_or_cross_Host_delivery".into()})
}
