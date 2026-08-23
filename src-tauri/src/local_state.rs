use std::{collections::HashMap, fs, path::PathBuf};

use chrono::Local;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

use crate::models::{MailConfig, ProcessingStatus};

const STATE_FILE: &str = "processing-state.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProcessingLedger {
    #[serde(default)]
    entries: HashMap<String, ProcessingEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessingEntry {
    status: String,
    processed_at: String,
}

pub fn state_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(STATE_FILE))
        .map_err(|e| format!("could not resolve local processing state directory: {e}"))
}

pub fn ensure_exists(app: &AppHandle) -> Result<PathBuf, String> {
    let path = state_path(app)?;
    if path.exists() {
        return Ok(path);
    }
    save(app, &ProcessingLedger::default())?;
    Ok(path)
}

pub fn entry_count(app: &AppHandle) -> Result<usize, String> {
    Ok(load(app)?.entries.len())
}

pub fn select_pending_uids(
    app: &AppHandle,
    mail: &MailConfig,
    uid_validity: u32,
    all_uids: &[u32],
    limit: usize,
) -> Result<(Vec<u32>, usize), String> {
    let ledger = load(app)?;
    let mut pending = all_uids
        .iter()
        .copied()
        .filter(|uid| {
            !ledger
                .entries
                .contains_key(&message_key(mail, uid_validity, *uid))
        })
        .collect::<Vec<_>>();
    let skipped_terminal = all_uids.len().saturating_sub(pending.len());
    if pending.len() > limit {
        pending = pending.split_off(pending.len() - limit);
    }
    Ok((pending, skipped_terminal))
}

pub fn mark_terminal(
    app: &AppHandle,
    mail: &MailConfig,
    uid_validity: u32,
    uid: u32,
    status: &ProcessingStatus,
) -> Result<(), String> {
    let status = match status {
        ProcessingStatus::Uploaded => "uploaded",
        ProcessingStatus::ProcessedNoAttachments => "processed_no_attachments",
        ProcessingStatus::NeedsReview => "needs_review",
        ProcessingStatus::Failed => return Ok(()),
    };

    let mut ledger = load(app)?;
    ledger.entries.insert(
        message_key(mail, uid_validity, uid),
        ProcessingEntry {
            status: status.to_string(),
            processed_at: Local::now().to_rfc3339(),
        },
    );
    save(app, &ledger)
}

fn load(app: &AppHandle) -> Result<ProcessingLedger, String> {
    let path = state_path(app)?;
    if !path.exists() {
        save(app, &ProcessingLedger::default())?;
        return Ok(ProcessingLedger::default());
    }
    let bytes = fs::read(&path)
        .map_err(|e| format!("could not read local processing state {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid local processing state {}: {e}", path.display()))
}

fn save(app: &AppHandle, ledger: &ProcessingLedger) -> Result<(), String> {
    let path = state_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("could not create local processing state directory: {e}"))?;
    }
    let bytes = serde_json::to_vec_pretty(ledger)
        .map_err(|e| format!("could not serialize local processing state: {e}"))?;

    // There is only one processor at a time (guarded by ProcessingGate), so a direct replace
    // avoids Windows rename-over-existing-file behavior while keeping this small ledger simple.
    fs::write(&path, bytes)
        .map_err(|e| format!("could not write local processing state {}: {e}", path.display()))
}

fn message_key(mail: &MailConfig, uid_validity: u32, uid: u32) -> String {
    let scope = format!(
        "{}\0{}\0{}\0{}\0{}\0{}",
        mail.host, mail.port, mail.username, mail.mailbox, uid_validity, uid
    );
    hex::encode(Sha256::digest(scope.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_key_changes_with_uid_validity() {
        let mail = MailConfig {
            host: "imap.example.com".into(),
            port: 993,
            username: "user@example.com".into(),
            mailbox: "INBOX".into(),
        };
        assert_ne!(message_key(&mail, 1, 42), message_key(&mail, 2, 42));
    }
}