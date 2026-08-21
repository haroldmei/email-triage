pub mod credentials;
pub mod extraction;
pub mod mail;
pub mod models;

use extraction::{DeterministicExtractor, IdentityExtractor};
use models::{MailConfig, ParsedMessage, StudentIdentity};

#[tauri::command]
async fn validate_imap(config: MailConfig, password: String) -> Result<(), String> {
    mail::validate_connection(&config, &password)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn extract_student_identity(message: ParsedMessage) -> StudentIdentity {
    DeterministicExtractor.extract(&message)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![validate_imap, extract_student_identity])
        .run(tauri::generate_context!())
        .expect("error while running Email Triage");
}
