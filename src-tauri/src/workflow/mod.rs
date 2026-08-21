use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    credentials::{CredentialStore, PlatformCredentialStore, MAIL_SERVICE},
    extraction::{DeterministicExtractor, IdentityExtractor},
    google_drive::{client_from_stored_refresh_token, DriveClient, DriveFile},
    mail::{self, parser::parse_message},
    models::{
        AppConfig, FetchedMessage, ParsedMessage, ProcessingResult, ProcessingStatus, StudentIdentity,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderMatch {
    Matched(DriveFile),
    Ambiguous(Vec<DriveFile>),
    NotFound,
}

pub async fn process_once(config: &AppConfig, limit: usize) -> Result<Vec<ProcessingResult>, WorkflowError> {
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
        .ok_or(WorkflowError::IncompleteConfig("Google Drive student root folder"))?;

    let password = PlatformCredentialStore
        .get(MAIL_SERVICE, &mail_config.username)
        .map_err(|e| WorkflowError::Credential(e.to_string()))?;
    let drive = client_from_stored_refresh_token(google_client_id, google_email)
        .await
        .map_err(|e| WorkflowError::Drive(e.to_string()))?;
    let folders = drive
        .list_folders(root_id)
        .await
        .map_err(|e| WorkflowError::Drive(e.to_string()))?;
    let messages = mail::fetch_unseen_messages(mail_config, &password, limit)
        .await
        .map_err(|e| WorkflowError::Mail(e.to_string()))?;

    let mut results = Vec::with_capacity(messages.len());
    for fetched in messages {
        results.push(
            process_message(config, &password, &drive, &folders, fetched).await,
        );
    }
    Ok(results)
}

async fn process_message(
    config: &AppConfig,
    password: &str,
    drive: &DriveClient,
    folders: &[DriveFile],
    fetched: FetchedMessage,
) -> ProcessingResult {
    let mail_config = config.mail.as_ref().expect("validated by process_once");
    let raw_digest = Sha256::digest(&fetched.raw);

    let message = match parse_message(&fetched.raw) {
        Ok(message) => message,
        Err(error) => {
            return review_result(
                config,
                password,
                fetched.uid,
                None,
                None,
                None,
                format!("Could not parse message: {error}"),
            )
            .await;
        }
    };

    let identity = DeterministicExtractor.extract(&message);
    let student_name = identity.name.as_ref().map(|value| value.value.clone());

    let folder = match match_student_folder(folders, &identity) {
        FolderMatch::Matched(folder) => folder,
        FolderMatch::Ambiguous(candidates) => {
            return review_result(
                config,
                password,
                fetched.uid,
                message.message_id.clone(),
                message.subject.clone(),
                student_name,
                format!(
                    "Student match is ambiguous across {} folders: {}",
                    candidates.len(),
                    candidates
                        .iter()
                        .map(|folder| folder.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .await;
        }
        FolderMatch::NotFound => {
            return review_result(
                config,
                password,
                fetched.uid,
                message.message_id.clone(),
                message.subject.clone(),
                student_name,
                "No unique student folder match was found".into(),
            )
            .await;
        }
    };

    let mut uploaded_file_ids = Vec::new();
    let mut skipped_existing_files = Vec::new();

    for attachment in &message.attachments {
        let key = attachment_key(&raw_digest, attachment.filename.as_bytes(), &attachment.bytes);
        match drive.find_by_triage_key(&folder.id, &key).await {
            Ok(existing) if !existing.is_empty() => {
                skipped_existing_files.push(attachment.filename.clone());
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                return failed_result(
                    fetched.uid,
                    &message,
                    student_name,
                    Some(folder.id.clone()),
                    uploaded_file_ids,
                    skipped_existing_files,
                    format!("Could not check Drive idempotency key: {error}"),
                );
            }
        }

        match drive.upload_attachment(&folder.id, attachment, &key).await {
            Ok(file) => uploaded_file_ids.push(file.id),
            Err(error) => {
                return failed_result(
                    fetched.uid,
                    &message,
                    student_name,
                    Some(folder.id.clone()),
                    uploaded_file_ids,
                    skipped_existing_files,
                    format!("Attachment upload failed: {error}"),
                );
            }
        }
    }

    if let Err(error) = mail::move_message(
        mail_config,
        password,
        fetched.uid,
        &config.processed_mailbox,
    )
    .await
    {
        return failed_result(
            fetched.uid,
            &message,
            student_name,
            Some(folder.id),
            uploaded_file_ids,
            skipped_existing_files,
            format!("Files are safe in Drive, but moving the source email failed: {error}"),
        );
    }

    let status = if message.attachments.is_empty() {
        ProcessingStatus::ProcessedNoAttachments
    } else {
        ProcessingStatus::Uploaded
    };
    ProcessingResult {
        uid: fetched.uid,
        message_id: message.message_id,
        subject: message.subject,
        student_name,
        folder_id: Some(folder.id),
        uploaded_file_ids,
        skipped_existing_files,
        status,
        detail: "Processing completed".into(),
    }
}

async fn review_result(
    config: &AppConfig,
    password: &str,
    uid: u32,
    message_id: Option<String>,
    subject: Option<String>,
    student_name: Option<String>,
    detail: String,
) -> ProcessingResult {
    let mail_config = config.mail.as_ref().expect("validated by process_once");
    match mail::move_message(mail_config, password, uid, &config.review_mailbox).await {
        Ok(()) => ProcessingResult {
            uid,
            message_id,
            subject,
            student_name,
            folder_id: None,
            uploaded_file_ids: Vec::new(),
            skipped_existing_files: Vec::new(),
            status: ProcessingStatus::NeedsReview,
            detail,
        },
        Err(error) => ProcessingResult {
            uid,
            message_id,
            subject,
            student_name,
            folder_id: None,
            uploaded_file_ids: Vec::new(),
            skipped_existing_files: Vec::new(),
            status: ProcessingStatus::Failed,
            detail: format!("{detail}; could not route to review mailbox: {error}"),
        },
    }
}

fn failed_result(
    uid: u32,
    message: &ParsedMessage,
    student_name: Option<String>,
    folder_id: Option<String>,
    uploaded_file_ids: Vec<String>,
    skipped_existing_files: Vec<String>,
    detail: String,
) -> ProcessingResult {
    ProcessingResult {
        uid,
        message_id: message.message_id.clone(),
        subject: message.subject.clone(),
        student_name,
        folder_id,
        uploaded_file_ids,
        skipped_existing_files,
        status: ProcessingStatus::Failed,
        detail,
    }
}

pub fn match_student_folder(folders: &[DriveFile], identity: &StudentIdentity) -> FolderMatch {
    if let Some(application_id) = identity.application_id.as_ref() {
        if application_id.confidence >= 0.95 {
            let needle = normalize(&application_id.value);
            if !needle.is_empty() {
                let matches = folders
                    .iter()
                    .filter(|folder| normalize(&folder.name).contains(&needle))
                    .cloned()
                    .collect::<Vec<_>>();
                match matches.len() {
                    1 => return FolderMatch::Matched(matches[0].clone()),
                    2.. => return FolderMatch::Ambiguous(matches),
                    _ => {}
                }
            }
        }
    }

    let Some(name) = identity.name.as_ref().filter(|name| name.confidence >= 0.9) else {
        return FolderMatch::NotFound;
    };
    let needle = normalize(&name.value);
    if needle.is_empty() {
        return FolderMatch::NotFound;
    }
    let matches = folders
        .iter()
        .filter(|folder| normalize(&folder.name).contains(&needle))
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
    use crate::models::ExtractedValue;

    fn folder(id: &str, name: &str) -> DriveFile {
        DriveFile {
            id: id.into(),
            name: name.into(),
            mime_type: Some("application/vnd.google-apps.folder".into()),
            parents: None,
        }
    }

    fn identity(name: &str, application_id: Option<&str>) -> StudentIdentity {
        StudentIdentity {
            name: Some(ExtractedValue {
                value: name.into(),
                confidence: 0.99,
                evidence: "test".into(),
            }),
            application_id: application_id.map(|value| ExtractedValue {
                value: value.into(),
                confidence: 0.99,
                evidence: "test".into(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn matches_numeric_prefix_student_folder_by_name() {
        let folders = vec![folder("1", "2654 Chang Rui"), folder("2", "Li Ming")];
        assert!(matches!(
            match_student_folder(&folders, &identity("Chang Rui", None)),
            FolderMatch::Matched(DriveFile { id, .. }) if id == "1"
        ));
    }

    #[test]
    fn application_id_is_preferred_when_name_is_ambiguous() {
        let folders = vec![
            folder("1", "2654 Chang Rui APP123"),
            folder("2", "3178 Chang Rui APP999"),
        ];
        assert!(matches!(
            match_student_folder(&folders, &identity("Chang Rui", Some("APP999"))),
            FolderMatch::Matched(DriveFile { id, .. }) if id == "2"
        ));
    }

    #[test]
    fn duplicate_name_is_never_auto_selected() {
        let folders = vec![folder("1", "2654 Chang Rui"), folder("2", "3178 Chang Rui")];
        assert!(matches!(
            match_student_folder(&folders, &identity("Chang Rui", None)),
            FolderMatch::Ambiguous(values) if values.len() == 2
        ));
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
