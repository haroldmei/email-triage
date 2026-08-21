pub mod config;
pub mod credentials;
pub mod extraction;
pub mod google_drive;
pub mod mail;
pub mod models;
pub mod scheduler;
pub mod workflow;

use tauri::{AppHandle, Manager, State};

use credentials::{CredentialStore, PlatformCredentialStore, MAIL_SERVICE};
use extraction::{DeterministicExtractor, IdentityExtractor};
use google_drive::DriveFile;
use models::{AppConfig, MailConfig, ParsedMessage, ProcessingResult, StudentIdentity};
use scheduler::ProcessingGate;

#[tauri::command]
fn get_config(app: AppHandle) -> Result<AppConfig, String> {
    config::load(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn save_config(app: AppHandle, app_config: AppConfig) -> Result<(), String> {
    config::save(&app, &app_config).map_err(|e| e.to_string())
}

#[tauri::command]
async fn save_mail_account(
    app: AppHandle,
    mail_config: MailConfig,
    password: String,
) -> Result<(), String> {
    mail_config.validate().map_err(|e| e.to_string())?;
    PlatformCredentialStore
        .set(MAIL_SERVICE, &mail_config.username, &password)
        .map_err(|e| e.to_string())?;

    if let Err(error) = mail::validate_connection(&mail_config, &password).await {
        let _ = PlatformCredentialStore.delete(MAIL_SERVICE, &mail_config.username);
        return Err(error.to_string());
    }

    let mut app_config = config::load(&app).map_err(|e| e.to_string())?;
    app_config.mail = Some(mail_config);
    config::save(&app, &app_config).map_err(|e| e.to_string())
}

#[tauri::command]
async fn validate_imap(app: AppHandle) -> Result<(), String> {
    let app_config = config::load(&app).map_err(|e| e.to_string())?;
    let mail_config = app_config
        .mail
        .ok_or_else(|| "Tencent mail account is not configured".to_string())?;
    let password = PlatformCredentialStore
        .get(MAIL_SERVICE, &mail_config.username)
        .map_err(|e| e.to_string())?;
    mail::validate_connection(&mail_config, &password)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn connect_google_account(
    app: AppHandle,
    client_id: String,
) -> Result<google_drive::GoogleConnection, String> {
    let connection = google_drive::connect_google(&client_id)
        .await
        .map_err(|e| e.to_string())?;
    let mut app_config = config::load(&app).map_err(|e| e.to_string())?;
    app_config.google_client_id = Some(client_id);
    app_config.google_email = Some(connection.email.clone());
    config::save(&app, &app_config).map_err(|e| e.to_string())?;
    Ok(connection)
}

#[tauri::command]
async fn list_drive_folders(app: AppHandle, parent_id: String) -> Result<Vec<DriveFile>, String> {
    let app_config = config::load(&app).map_err(|e| e.to_string())?;
    let client_id = app_config
        .google_client_id
        .as_deref()
        .ok_or_else(|| "Google OAuth client ID is not configured".to_string())?;
    let email = app_config
        .google_email
        .as_deref()
        .ok_or_else(|| "Google account is not connected".to_string())?;
    let drive = google_drive::client_from_stored_refresh_token(client_id, email)
        .await
        .map_err(|e| e.to_string())?;
    drive
        .list_folders(&parent_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_drive_root(app: AppHandle, folder_id: String) -> Result<(), String> {
    if folder_id.trim().is_empty() {
        return Err("Drive root folder ID is required".into());
    }
    let mut app_config = config::load(&app).map_err(|e| e.to_string())?;
    app_config.drive_root_id = Some(folder_id);
    config::save(&app, &app_config).map_err(|e| e.to_string())
}

#[tauri::command]
async fn process_now(
    app: AppHandle,
    gate: State<'_, ProcessingGate>,
) -> Result<Vec<ProcessingResult>, String> {
    let gate = gate.inner().clone();
    if !scheduler::try_enter(&gate) {
        return Err("Email processing is already running".into());
    }

    let result = match config::load(&app) {
        Ok(app_config) => workflow::process_once(&app_config, 100)
            .await
            .map_err(|e| e.to_string()),
        Err(error) => Err(error.to_string()),
    };
    scheduler::leave(&gate);
    result
}

#[tauri::command]
fn extract_student_identity(message: ParsedMessage) -> StudentIdentity {
    DeterministicExtractor.extract(&message)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(scheduler::new_gate())
        .setup(|app| {
            scheduler::start(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            save_mail_account,
            validate_imap,
            connect_google_account,
            list_drive_folders,
            set_drive_root,
            process_now,
            extract_student_identity
        ])
        .run(tauri::generate_context!())
        .expect("error while running Email Triage");
}
