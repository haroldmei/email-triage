pub mod credentials;
pub mod extraction;
pub mod mail;
pub mod models;

use credentials::{CredentialStore, PlatformCredentialStore, MAIL_SERVICE};
use extraction::{DeterministicExtractor, IdentityExtractor};
use models::{MailConfig, ParsedMessage, StudentIdentity};

#[tauri::command]
fn save_mail_password(account: String, password: String) -> Result<(), String> {
    PlatformCredentialStore
        .set(MAIL_SERVICE, &account, &password)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn validate_imap(config: MailConfig) -> Result<(), String> {
    let password = PlatformCredentialStore
        .get(MAIL_SERVICE, &config.username)
        .map_err(|e| e.to_string())?;
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
        .invoke_handler(tauri::generate_handler![
            save_mail_password,
            validate_imap,
            extract_student_identity
        ])
        .run(tauri::generate_context!())
        .expect("error while running Email Triage");
}
