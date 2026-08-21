pub mod config;
pub mod credentials;
pub mod extraction;
pub mod google_drive;
pub mod logging;
pub mod mail;
pub mod models;
pub mod scheduler;
pub mod workflow;

use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tokio::time::timeout;

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
fn get_log_path(app: AppHandle) -> Result<String, String> {
    logging::log_path(&app).map(|path| path.display().to_string())
}

#[tauri::command]
fn get_recent_logs(app: AppHandle, max_lines: usize) -> Result<Vec<String>, String> {
    logging::read_recent(&app, max_lines)
}

#[tauri::command]
async fn process_now(
    app: AppHandle,
    gate: State<'_, ProcessingGate>,
) -> Result<Vec<ProcessingResult>, String> {
    let gate = gate.inner().clone();
    let Some(_lease) = scheduler::try_enter(&gate, "manual") else {
        let source = scheduler::current_source(&gate).unwrap_or_else(|| "unknown".into());
        return Err(format!("Email processing is already running ({source})"));
    };

    let app_config = match config::load(&app) {
        Ok(config) => config,
        Err(error) => {
            logging::write(
                &app,
                "ERROR",
                format!("source=manual configuration_load failed error=\"{error}\""),
            );
            return Err(error.to_string());
        }
    };
    let mailbox = app_config
        .mail
        .as_ref()
        .map(|mail| mail.mailbox.as_str())
        .unwrap_or("unknown");
    logging::write(
        &app,
        "INFO",
        format!("source=manual mailbox_check mailbox=\"{mailbox}\" started"),
    );

    let result = timeout(Duration::from_secs(120), workflow::process_once(&app_config, 100)).await;
    match result {
        Ok(Ok(results)) => {
            logging::write_processing_results(&app, "manual", &app_config, &results);
            Ok(results)
        }
        Ok(Err(error)) => {
            logging::write(
                &app,
                "ERROR",
                format!(
                    "source=manual mailbox_check mailbox=\"{mailbox}\" failed error=\"{error}\""
                ),
            );
            Err(error.to_string())
        }
        Err(_) => {
            logging::write(
                &app,
                "ERROR",
                format!(
                    "source=manual mailbox_check mailbox=\"{mailbox}\" timed_out seconds=120"
                ),
            );
            Err("Email processing timed out after 120 seconds. Check the local app log for the last completed stage.".into())
        }
    }
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    if enabled {
        app.autolaunch().enable().map_err(|e| e.to_string())
    } else {
        app.autolaunch().disable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn extract_student_identity(message: ParsedMessage) -> StudentIdentity {
    DeterministicExtractor.extract(&message)
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open Email Triage", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Email Triage")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(scheduler::new_gate())
        .setup(|app| {
            app.handle().plugin(tauri_plugin_autostart::init(
                MacosLauncher::LaunchAgent,
                None,
            ))?;
            logging::write(
                app.handle(),
                "INFO",
                "Email Triage started processing_state=idle",
            );
            setup_tray(app)?;
            scheduler::start(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            save_mail_account,
            validate_imap,
            connect_google_account,
            list_drive_folders,
            set_drive_root,
            get_log_path,
            get_recent_logs,
            process_now,
            get_autostart,
            set_autostart,
            extract_student_identity
        ])
        .run(tauri::generate_context!())
        .expect("error while running Email Triage");
}
