use std::time::Instant;

use sha2::{Digest, Sha256};
use tauri::AppHandle;
use thiserror::Error;

use crate::{
    credentials::{CredentialStore, PlatformCredentialStore, MAIL_SERVICE},
    extraction::{DeterministicExtractor, IdentityExtractor},
    google_drive::{client_from_stored_refresh_token, DriveClient, DriveFile},
    local_state, logging,
    mail::{self, parser::parse_message},
    models::{
        AppConfig, ExtractedValue, FetchedMessage, ParsedMessage, ProcessingResult,
        ProcessingStatus, StudentIdentity,
    },
};

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("configuration is incomplete: {0}")]
    IncompleteConfig(&'static str),
    #[error("mail credential error: {0}")]
    Credential(String),
    #[error("mail error: {0}")]
    Mail(String),
    #[error("Google Drive error: {0}")]
    Drive(String),
    #[error("local processing state error: {0}")]
    LocalState(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderMatch {
    Matched(DriveFile),
    Ambiguous(Vec<DriveFile>),
    NotFound,
}

pub async fn process_once(
    app: &AppHandle,
    config: &AppConfig,
    limit: usize,
) -> Result<Vec<ProcessingResult>, WorkflowError> {
    let run_started = Instant::now();
    let mail_config = config
        .mail
        .as_ref()
        .ok_or(WorkflowError::IncompleteConfig("Tencent mail account"))?;
    let google_client_id = config
        .google_client_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or(WorkflowError::IncompleteConfig("Google OAuth client ID"))?;
    let google_email = config
        .google_email
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or(WorkflowError::IncompleteConfig("Google account"))?;
    let root_id = config
        .drive_root_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or(WorkflowError::IncompleteConfig(
            "Google Drive student root folder",
        ))?;

    let password = PlatformCredentialStore
        .get(MAIL_SERVICE, &mail_config.username)
        .map_err(|e| WorkflowError::Credential(e.to_string()))?;

    let scan_started = Instant::now();
    logging::write(
        app,
        "INFO",
        format!("stage=mailbox_scan mailbox=\"{}\" started", mail_config.mailbox),
    );
    let listing = mail::list_message_uids(mail_config, &password)
        .await
        .map_err(|e| WorkflowError::Mail(e.to_string()))?;
    let (pending_uids, skipped_local) = local_state::select_pending_uids(
        app,
        mail_config,
        listing.uid_validity,
        &listing.uids,
        limit,
    )
    .map_err(WorkflowError::LocalState)?;

    logging::write(
        app,
        "INFO",
        format!(
            "stage=mailbox_scan mailbox=\"{}\" completed total_messages={} known_local={} recent_window={} candidate_messages={} recent_uid_first={} recent_uid_last={} candidate_uid_first={} candidate_uid_last={} uid_validity={} elapsed_ms={}",
            mail_config.mailbox,
            listing.total_messages,
            skipped_local,
            listing.uids.len(),
            pending_uids.len(),
            listing.uids.first().copied().unwrap_or_default(),
            listing.uids.last().copied().unwrap_or_default(),
            pending_uids.first().copied().unwrap_or_default(),
            pending_uids.last().copied().unwrap_or_default(),
            listing.uid_validity,
            scan_started.elapsed().as_millis()
        ),
    );

    if pending_uids.is_empty() {
        logging::write(
            app,
            "INFO",
            format!("stage=process_once completed candidate_messages=0 elapsed_ms={}", run_started.elapsed().as_millis()),
        );
        return Ok(Vec::new());
    }

    let fetch_started = Instant::now();
    let batch = mail::fetch_messages_by_uid(
        app,
        mail_config,
        &password,
        listing.uid_validity,
        &pending_uids,
    )
    .await
    .map_err(|e| WorkflowError::Mail(e.to_string()))?;
    logging::write(
        app,
        "INFO",
        format!(
            "stage=message_fetch_batch completed requested={} fetched={} failed={} elapsed_ms={}",
            pending_uids.len(),
            batch.messages.len(),
            batch.failures.len(),
            fetch_started.elapsed().as_millis()
        ),
    );

    let mut results = batch
        .failures
        .into_iter()
        .map(|failure| fetch_failure_result(failure.uid, failure.error))
        .collect::<Vec<_>>();

    // Parse first so messages without attachments can be completed locally without requiring Drive.
    let mut parsed = Vec::with_capacity(batch.messages.len());
    for fetched in batch.messages {
        let parse_started = Instant::now();
        match parse_message(&fetched.raw) {
            Ok(message) => {
                logging::write(
                    app,
                    "INFO",
                    format!(
                        "stage=mime_parse uid={} completed attachments={} elapsed_ms={}",
                        fetched.uid,
                        message.attachments.len(),
                        parse_started.elapsed().as_millis()
                    ),
                );
                if message.attachments.is_empty() {
                    results.push(no_attachment_result(fetched.uid, message));
                } else {
                    parsed.push((fetched, message));
                }
            }
            Err(error) => {
                logging::write(
                    app,
                    "WARN",
                    format!(
                        "stage=mime_parse uid={} failed elapsed_ms={} error=\"{}\"",
                        fetched.uid,
                        parse_started.elapsed().as_millis(),
                        error
                    ),
                );
                results.push(review_result(
                    fetched.uid,
                    None,
                    None,
                    None,
                    0,
                    format!("Could not parse message: {error}"),
                ));
            }
        }
    }

    if !parsed.is_empty() {
        let drive_auth_started = Instant::now();
        logging::write(app, "INFO", "stage=drive_connect started");
        let drive = client_from_stored_refresh_token(google_client_id, google_email)
            .await
            .map_err(|e| WorkflowError::Drive(e.to_string()))?;
        logging::write(
            app,
            "INFO",
            format!("stage=drive_connect completed elapsed_ms={}", drive_auth_started.elapsed().as_millis()),
        );

        let folder_started = Instant::now();
        logging::write(
            app,
            "INFO",
            format!("stage=drive_folder_list root_id=\"{root_id}\" started"),
        );
        let folders = drive
            .list_folders(root_id)
            .await
            .map_err(|e| WorkflowError::Drive(e.to_string()))?;
        logging::write(
            app,
            "INFO",
            format!(
                "stage=drive_folder_list root_id=\"{root_id}\" completed folders={} elapsed_ms={}",
                folders.len(),
                folder_started.elapsed().as_millis()
            ),
        );
        for (fetched, message) in parsed {
            results.push(process_parsed_message(app, &drive, &folders, fetched, message).await);
        }
    }

    results.sort_by_key(|result| result.uid);
    for result in &results {
        if !matches!(result.status, ProcessingStatus::Failed) {
            local_state::mark_terminal(
                app,
                mail_config,
                listing.uid_validity,
                result.uid,
                &result.status,
            )
            .map_err(WorkflowError::LocalState)?;
        }
    }
    logging::write(
        app,
        "INFO",
        format!(
            "stage=process_once completed results={} failed={} elapsed_ms={}",
            results.len(),
            results.iter().filter(|result| matches!(result.status, ProcessingStatus::Failed)).count(),
            run_started.elapsed().as_millis()
        ),
    );
    Ok(results)
}

fn fetch_failure_result(uid: u32, error: String) -> ProcessingResult {
    ProcessingResult {
        uid,
        message_id: None,
        subject: None,
        student_name: None,
        folder_id: None,
        folder_name: None,
        attachment_count: 0,
        uploaded_file_ids: Vec::new(),
        uploaded_file_names: Vec::new(),
        skipped_existing_files: Vec::new(),
        status: ProcessingStatus::Failed,
        detail: format!("Could not fetch message; retryable: {error}"),
    }
}

fn no_attachment_result(uid: u32, message: ParsedMessage) -> ProcessingResult {
    let identity = DeterministicExtractor.extract(&message);
    let student_name = preferred_student_name(&identity).map(|value| value.value.clone());
    ProcessingResult {
        uid,
        message_id: message.message_id,
        subject: message.subject,
        student_name,
        folder_id: None,
        folder_name: None,
        attachment_count: 0,
        uploaded_file_ids: Vec::new(),
        uploaded_file_names: Vec::new(),
        skipped_existing_files: Vec::new(),
        status: ProcessingStatus::ProcessedNoAttachments,
        detail: "No attachments found; source email left unchanged".into(),
    }
}

async fn process_parsed_message(
    app: &AppHandle,
    drive: &DriveClient,
    folders: &[DriveFile],
    fetched: FetchedMessage,
    message: ParsedMessage,
) -> ProcessingResult {
    let message_started = Instant::now();
    let raw_digest = Sha256::digest(&fetched.raw);
    let identity = DeterministicExtractor.extract(&message);
    let student_name = preferred_student_name(&identity).map(|value| value.value.clone());
    let attachment_count = message.attachments.len();

    logging::write(
        app,
        "INFO",
        format!(
            "stage=student_match uid={} started attachments={} student=\"{}\"",
            fetched.uid,
            attachment_count,
            student_name.as_deref().unwrap_or("unknown")
        ),
    );
    let folder = match match_student_folder(folders, &identity) {
        FolderMatch::Matched(folder) => {
            logging::write(
                app,
                "INFO",
                format!("stage=student_match uid={} matched folder=\"{}\" folder_id=\"{}\"", fetched.uid, folder.name, folder.id),
            );
            folder
        }
        FolderMatch::Ambiguous(candidates) => {
            logging::write(
                app,
                "WARN",
                format!("stage=student_match uid={} ambiguous candidates={}", fetched.uid, candidates.len()),
            );
            return review_result(
                fetched.uid,
                message.message_id.clone(),
                message.subject.clone(),
                student_name,
                attachment_count,
                format!(
                    "Student match is ambiguous across {} folders: {}",
                    candidates.len(),
                    candidates
                        .iter()
                        .map(|folder| folder.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        FolderMatch::NotFound => {
            logging::write(app, "WARN", format!("stage=student_match uid={} not_found", fetched.uid));
            return review_result(
                fetched.uid,
                message.message_id.clone(),
                message.subject.clone(),
                student_name,
                attachment_count,
                "No unique student folder match was found".into(),
            );
        }
    };

    let mut uploaded_file_ids = Vec::new();
    let mut uploaded_file_names = Vec::new();
    let mut skipped_existing_files = Vec::new();

    for (attachment_index, attachment) in message.attachments.iter().enumerate() {
        let attachment_started = Instant::now();
        logging::write(
            app,
            "INFO",
            format!(
                "stage=attachment uid={} sequence={}/{} filename=\"{}\" bytes={} started",
                fetched.uid,
                attachment_index + 1,
                attachment_count,
                attachment.filename,
                attachment.bytes.len()
            ),
        );
        let key = attachment_key(&raw_digest, attachment.filename.as_bytes(), &attachment.bytes);
        match drive.find_by_triage_key(&folder.id, &key).await {
            Ok(existing) if !existing.is_empty() => {
                skipped_existing_files.push(attachment.filename.clone());
                logging::write(
                    app,
                    "INFO",
                    format!("stage=attachment uid={} filename=\"{}\" skipped_existing=true elapsed_ms={}", fetched.uid, attachment.filename, attachment_started.elapsed().as_millis()),
                );
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                logging::write(
                    app,
                    "ERROR",
                    format!("stage=attachment uid={} filename=\"{}\" idempotency_check_failed elapsed_ms={} error=\"{}\"", fetched.uid, attachment.filename, attachment_started.elapsed().as_millis(), error),
                );
                return failed_result(
                    fetched.uid,
                    &message,
                    student_name,
                    Some(folder.id.clone()),
                    Some(folder.name.clone()),
                    attachment_count,
                    uploaded_file_ids,
                    uploaded_file_names,
                    skipped_existing_files,
                    format!("Could not check Drive idempotency key: {error}"),
                );
            }
        }

        match drive.upload_attachment(&folder.id, attachment, &key).await {
            Ok(file) => {
                uploaded_file_ids.push(file.id);
                uploaded_file_names.push(attachment.filename.clone());
                logging::write(
                    app,
                    "INFO",
                    format!("stage=attachment uid={} filename=\"{}\" uploaded=true elapsed_ms={}", fetched.uid, attachment.filename, attachment_started.elapsed().as_millis()),
                );
            }
            Err(error) => {
                logging::write(
                    app,
                    "ERROR",
                    format!("stage=attachment uid={} filename=\"{}\" upload_failed elapsed_ms={} error=\"{}\"", fetched.uid, attachment.filename, attachment_started.elapsed().as_millis(), error),
                );
                return failed_result(
                    fetched.uid,
                    &message,
                    student_name,
                    Some(folder.id.clone()),
                    Some(folder.name.clone()),
                    attachment_count,
                    uploaded_file_ids,
                    uploaded_file_names,
                    skipped_existing_files,
                    format!("Attachment upload failed: {error}"),
                );
            }
        }
    }

    logging::write(
        app,
        "INFO",
        format!("stage=message_processing uid={} completed attachments={} elapsed_ms={}", fetched.uid, attachment_count, message_started.elapsed().as_millis()),
    );
    ProcessingResult {
        uid: fetched.uid,
        message_id: message.message_id,
        subject: message.subject,
        student_name,
        folder_id: Some(folder.id),
        folder_name: Some(folder.name),
        attachment_count,
        uploaded_file_ids,
        uploaded_file_names,
        skipped_existing_files,
        status: ProcessingStatus::Uploaded,
        detail: "Processing completed; source email left unchanged".into(),
    }
}

fn review_result(
    uid: u32,
    message_id: Option<String>,
    subject: Option<String>,
    student_name: Option<String>,
    attachment_count: usize,
    detail: String,
) -> ProcessingResult {
    ProcessingResult {
        uid,
        message_id,
        subject,
        student_name,
        folder_id: None,
        folder_name: None,
        attachment_count,
        uploaded_file_ids: Vec::new(),
        uploaded_file_names: Vec::new(),
        skipped_existing_files: Vec::new(),
        status: ProcessingStatus::NeedsReview,
        detail: format!("{detail}; source email left unchanged"),
    }
}

#[allow(clippy::too_many_arguments)]
fn failed_result(
    uid: u32,
    message: &ParsedMessage,
    student_name: Option<String>,
    folder_id: Option<String>,
    folder_name: Option<String>,
    attachment_count: usize,
    uploaded_file_ids: Vec<String>,
    uploaded_file_names: Vec<String>,
    skipped_existing_files: Vec<String>,
    detail: String,
) -> ProcessingResult {
    ProcessingResult {
        uid,
        message_id: message.message_id.clone(),
        subject: message.subject.clone(),
        student_name,
        folder_id,
        folder_name,
        attachment_count,
        uploaded_file_ids,
        uploaded_file_names,
        skipped_existing_files,
        status: ProcessingStatus::Failed,
        detail,
    }
}

fn preferred_student_name(identity: &StudentIdentity) -> Option<&ExtractedValue> {
    identity
        .chinese_name
        .as_ref()
        .filter(|value| value.confidence >= 0.9)
        .or_else(|| {
            identity
                .english_name
                .as_ref()
                .filter(|value| value.confidence >= 0.9)
        })
        .or_else(|| identity.name.as_ref().filter(|value| value.confidence >= 0.9))
}

pub fn match_student_folder(folders: &[DriveFile], identity: &StudentIdentity) -> FolderMatch {
    let Some(name) = preferred_student_name(identity) else {
        return FolderMatch::NotFound;
    };
    let needle = normalize(&name.value);
    if needle.is_empty() {
        return FolderMatch::NotFound;
    }

    let matches = folders
        .iter()
        .filter(|folder| normalize(&folder.name) == needle)
        .cloned()
        .collect::<Vec<_>>();

    match matches.len() {
        0 => FolderMatch::NotFound,
        1 => FolderMatch::Matched(matches[0].clone()),
        _ => FolderMatch::Ambiguous(matches),
    }
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn attachment_key(raw_digest: &[u8], filename: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_digest);
    hasher.update([0]);
    hasher.update(filename);
    hasher.update([0]);
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, name: &str) -> DriveFile {
        DriveFile {
            id: id.into(),
            name: name.into(),
            mime_type: Some("application/vnd.google-apps.folder".into()),
            parents: None,
        }
    }

    fn extracted(value: &str) -> ExtractedValue {
        ExtractedValue {
            value: value.into(),
            confidence: 0.99,
            evidence: "test".into(),
        }
    }

    #[test]
    fn chinese_name_is_preferred_over_english_name() {
        let identity = StudentIdentity {
            name: Some(extracted("常瑞")),
            english_name: Some(extracted("Chang Rui")),
            chinese_name: Some(extracted("常瑞")),
            ..Default::default()
        };
        assert_eq!(preferred_student_name(&identity).unwrap().value, "常瑞");
    }

    #[test]
    fn matches_chinese_student_folder_when_both_names_exist() {
        let folders = vec![folder("1", "常瑞"), folder("2", "Chang Rui")];
        let identity = StudentIdentity {
            name: Some(extracted("常瑞")),
            english_name: Some(extracted("Chang Rui")),
            chinese_name: Some(extracted("常瑞")),
            ..Default::default()
        };
        assert!(matches!(
            match_student_folder(&folders, &identity),
            FolderMatch::Matched(DriveFile { id, .. }) if id == "1"
        ));
    }

    #[test]
    fn falls_back_to_english_when_chinese_name_is_missing() {
        let folders = vec![folder("1", "Chang Rui"), folder("2", "Li Ming")];
        let identity = StudentIdentity {
            name: Some(extracted("Chang Rui")),
            english_name: Some(extracted("Chang Rui")),
            chinese_name: None,
            ..Default::default()
        };
        assert!(matches!(
            match_student_folder(&folders, &identity),
            FolderMatch::Matched(DriveFile { id, .. }) if id == "1"
        ));
    }

    #[test]
    fn does_not_use_english_folder_when_chinese_name_exists() {
        let folders = vec![folder("1", "Chang Rui")];
        let identity = StudentIdentity {
            name: Some(extracted("常瑞")),
            english_name: Some(extracted("Chang Rui")),
            chinese_name: Some(extracted("常瑞")),
            ..Default::default()
        };
        assert!(matches!(
            match_student_folder(&folders, &identity),
            FolderMatch::NotFound
        ));
    }

    #[test]
    fn duplicate_student_folder_name_is_never_auto_selected() {
        let folders = vec![folder("1", "常瑞"), folder("2", "常瑞")];
        let identity = StudentIdentity {
            name: Some(extracted("常瑞")),
            chinese_name: Some(extracted("常瑞")),
            ..Default::default()
        };
        assert!(matches!(
            match_student_folder(&folders, &identity),
            FolderMatch::Ambiguous(values) if values.len() == 2
        ));
    }

    #[test]
    fn no_attachment_message_is_terminal_without_folder_match() {
        let result = no_attachment_result(
            7,
            ParsedMessage {
                subject: Some("FYI".into()),
                ..Default::default()
            },
        );
        assert_eq!(result.status, ProcessingStatus::ProcessedNoAttachments);
        assert_eq!(result.attachment_count, 0);
        assert!(result.folder_id.is_none());
    }

    #[test]
    fn fetch_failure_is_retryable() {
        let result = fetch_failure_result(42, "operation timed out: read message fetch".into());
        assert_eq!(result.uid, 42);
        assert_eq!(result.status, ProcessingStatus::Failed);
        assert!(result.detail.contains("retryable"));
    }

    #[test]
    fn attachment_key_is_deterministic() {
        let digest = Sha256::digest(b"message");
        assert_eq!(
            attachment_key(&digest, b"offer.pdf", b"payload"),
            attachment_key(&digest, b"offer.pdf", b"payload")
        );
        assert_ne!(
            attachment_key(&digest, b"offer.pdf", b"payload"),
            attachment_key(&digest, b"offer.pdf", b"new payload")
        );
    }
}
