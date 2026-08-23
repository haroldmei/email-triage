pub mod config;
pub mod credentials;
pub mod extraction;
pub mod google_drive;
pub mod local_state;
pub mod logging;
pub mod mail;
pub mod models;
pub mod scheduler;
pub mod workflow;

use std::time::Instant;

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
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

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
fn get_processing_state_path(app: AppHandle) -> Result<String, String> {
    local_state::ensure_exists(&app).map(|path| path.display().to_string())
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
    let version = env!("CARGO_PKG_VERSION");
    let pid = std::process::id();
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
                format!("version={version} pid={pid} source=manual stage=configuration_load action=\"Loading saved configuration before manual mailbox check\" failed=true error=\"{error}\""),
            );
            return Err(error.to_string());
        }
    };
    let mailbox = app_config
        .mail
        .as_ref()
        .map(|mail| mail.mailbox.as_str())
        .unwrap_or("unknown");
    let started = Instant::now();
    logging::write(
        &app,
        "INFO",
        format!(
            "version={version} pid={pid} source=manual mailbox_check mailbox=\"{mailbox}\" started watchdog_seconds={} action=\"Checking Tencent mailbox, fetching candidate messages, extracting student identity, and filing attachments to Drive\"",
            scheduler::PROCESSING_TIMEOUT.as_secs()
        ),
    );

    let result = timeout(
        scheduler::PROCESSING_TIMEOUT,
        workflow::process_once(&app, &app_config, 100),
    )
    .await;
    match result {
        Ok(Ok(results)) => {
            logging::write_processing_results(&app, "manual", &results);
            logging::write(
                &app,
                "INFO",
                format!(
                    "version={version} pid={pid} source=manual mailbox_check mailbox=\"{mailbox}\" completed elapsed_ms={} action=\"Manual mailbox check finished\"",
                    started.elapsed().as_millis()
                ),
            );
            Ok(results)
        }
        Ok(Err(error)) => {
            logging::write(
                &app,
                "ERROR",
                format!(
                    "version={version} pid={pid} source=manual mailbox_check mailbox=\"{mailbox}\" failed elapsed_ms={} action=\"Manual mailbox check stopped with an error\" error=\"{error}\"",
                    started.elapsed().as_millis()
                ),
            );
            Err(error.to_string())
        }
        Err(_) => {
            logging::write(
                &app,
                "ERROR",
                format!(
                    "version={version} pid={pid} source=manual mailbox_check mailbox=\"{mailbox}\" timed_out seconds={} elapsed_ms={} action=\"Manual mailbox check exceeded its overall watchdog timeout\"",
                    scheduler::PROCESSING_TIMEOUT.as_secs(),
                    started.elapsed().as_millis()
                ),
            );
            Err(format!(
                "Email processing timed out after {} seconds. Check the local app log for the last completed stage.",
                scheduler::PROCESSING_TIMEOUT.as_secs()
            ))
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

fn panic_payload(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).replace('"', "'")
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.replace('"', "'")
    } else {
        "non-string panic payload".into()
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            let version = env!("CARGO_PKG_VERSION");
            let pid = std::process::id();
            logging::write(
                app,
                "WARN",
                format!(
                    "version={version} pid={pid} stage=single_instance second_launch_blocked=true args_count={} cwd=\"{}\" action=\"A second Email Triage launch was prevented; focusing the already-running instance\"",
                    args.len(),
                    cwd.replace('"', "'")
                ),
            );
            show_main_window(app);
        }))
        .manage(scheduler::new_gate())
        .setup(|app| {
            app.handle().plugin(tauri_plugin_autostart::init(
                MacosLauncher::LaunchAgent,
                None,
            ))?;

            let panic_app = app.handle().clone();
            std::panic::set_hook(Box::new(move |info| {
                let version = env!("CARGO_PKG_VERSION");
                let pid = std::process::id();
                let thread = std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .replace('"', "'");
                let location = info
                    .location()
                    .map(|location| format!("{}:{}:{}", location.file(), location.line(), location.column()))
                    .unwrap_or_else(|| "unknown".into())
                    .replace('"', "'");
                logging::write(
                    &panic_app,
                    "ERROR",
                    format!(
                        "version={version} pid={pid} stage=panic thread=\"{thread}\" location=\"{location}\" message=\"{}\" action=\"A Rust panic occurred; inspect the immediately preceding stage/UID logs\"",
                        panic_payload(info)
                    ),
                );
            }));

            let (state_path, state_exists, state_entries) =
                match local_state::ensure_exists(app.handle()) {
                    Ok(path) => {
                        let entries = local_state::entry_count(app.handle()).unwrap_or_default();
                        (path.display().to_string(), true, entries)
                    }
                    Err(error) => {
                        logging::write(
                            app.handle(),
                            "ERROR",
                            format!("version={} pid={} stage=local_state action=\"Initializing local processing state\" initialize_failed=true error=\"{error}\"", env!("CARGO_PKG_VERSION"), std::process::id()),
                        );
                        ("unavailable".into(), false, 0)
                    }
                };

            let executable = std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "unknown".into())
                .replace('"', "'");
            logging::write(
                app.handle(),
                "INFO",
                format!(
                    "Email Triage started version={} pid={} executable=\"{executable}\" single_instance=true action=\"Application startup complete; background scheduler is active\" processing_state=idle mail_access=read_only local_state=\"{state_path}\" local_state_exists={state_exists} local_state_entries={state_entries} message_fetch_timeout_seconds={} processing_watchdog_seconds={}",
                    env!("CARGO_PKG_VERSION"),
                    std::process::id(),
                    mail::MESSAGE_FETCH_TIMEOUT.as_secs(),
                    scheduler::PROCESSING_TIMEOUT.as_secs()
                ),
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
            get_app_version,
            get_config,
            save_config,
            save_mail_account,
            validate_imap,
            connect_google_account,
            list_drive_folders,
            set_drive_root,
            get_log_path,
            get_processing_state_path,
            get_recent_logs,
            process_now,
            get_autostart,
            set_autostart,
            extract_student_identity
        ])
        .run(tauri::generate_context!())
        .expect("error while running Email Triage");
}
