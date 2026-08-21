use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use tauri::{AppHandle, Manager};

const LOG_FILE: &str = "email-triage.log";

pub fn log_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join("logs").join(LOG_FILE))
        .map_err(|e| format!("could not resolve app log directory: {e}"))
}

pub fn write(app: &AppHandle, level: &str, message: impl AsRef<str>) {
    let Ok(path) = log_path(app) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let message = message.as_ref().replace('\r', " ").replace('\n', " ");
    let _ = writeln!(file, "{timestamp} [{level}] {message}");
}
