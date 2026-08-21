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
    pub sequence: u32,
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
    pub name: Option<ExtractedValue>,
    pub application_id: Option<ExtractedValue>,
    pub date_of_birth: Option<ExtractedValue>,
    pub university: Option<ExtractedValue>,
    pub course: Option<ExtractedValue>,
}
