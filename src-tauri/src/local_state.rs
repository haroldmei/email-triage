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

pub fn is_terminal(
    app: &AppHandle,
    mail: &MailConfig,
    uid_validity: u32,
    uid: u32,
) -> Result<bool, String> {
    let ledger = load(app)?;
    Ok(ledger.entries.contains_key(&message_key(
        mail,
        uid_validity,
        uid,
    )))
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
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes)
        .map_err(|e| format!("could not write local processing state: {e}"))?;
    fs::rename(&temp, &path)
        .map_err(|e| format!("could not replace local processing state: {e}"))
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
