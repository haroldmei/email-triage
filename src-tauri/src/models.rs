use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MailConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default = "default_mailbox")]
    pub mailbox: String,
}

fn default_mailbox() -> String {
    "INBOX".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FetchedMessage {
    pub uid: u32,
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ParsedMessage {
    pub subject: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub date: Option<String>,
    pub message_id: Option<String>,
    pub text_body: String,
    pub html_body: String,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedValue {
    pub value: String,
    pub confidence: f32,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StudentIdentity {
    /// Best available name retained for backwards compatibility and display.
    pub name: Option<ExtractedValue>,
    pub english_name: Option<ExtractedValue>,
    pub chinese_name: Option<ExtractedValue>,
    pub application_id: Option<ExtractedValue>,
    pub date_of_birth: Option<ExtractedValue>,
    pub university: Option<ExtractedValue>,
    pub course: Option<ExtractedValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub mail: Option<MailConfig>,
    pub google_client_id: Option<String>,
    pub google_email: Option<String>,
    pub drive_root_id: Option<String>,
    pub poll_interval_seconds: u64,
    pub processed_mailbox: String,
    pub review_mailbox: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            mail: None,
            google_client_id: None,
            google_email: None,
            drive_root_id: None,
            poll_interval_seconds: 60,
            processed_mailbox: "EmailTriage-Processed".to_string(),
            review_mailbox: "EmailTriage-NeedsReview".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProcessingStatus {
    Uploaded,
    ProcessedNoAttachments,
    NeedsReview,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingResult {
    pub uid: u32,
    pub message_id: Option<String>,
    pub subject: Option<String>,
    pub student_name: Option<String>,
    pub folder_id: Option<String>,
    pub uploaded_file_ids: Vec<String>,
    pub skipped_existing_files: Vec<String>,
    pub status: ProcessingStatus,
    pub detail: String,
}
