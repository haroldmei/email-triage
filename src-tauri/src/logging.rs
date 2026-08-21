use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use chrono::Local;
use tauri::{AppHandle, Manager};

use crate::models::{AppConfig, ProcessingResult, ProcessingStatus};

const LOG_FILE: &str = "email-triage.log";
const ROTATED_LOG_FILE: &str = "email-triage.log.1";
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const MAX_UI_LOG_LINES: usize = 500;

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
    rotate_if_needed(&path);
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%:z");
    let message = message.as_ref().replace(['\r', '\n'], " ");
    let _ = writeln!(file, "{timestamp} [{level}] {message}");
}

pub fn read_recent(app: &AppHandle, max_lines: usize) -> Result<Vec<String>, String> {
    let path = log_path(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("could not read app log {}: {e}", path.display()))?;
    let max_lines = max_lines.clamp(1, MAX_UI_LOG_LINES);
    let mut lines = content
        .lines()
        .rev()
        .take(max_lines)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    lines.reverse();
    Ok(lines)
}

pub fn write_processing_results(
    app: &AppHandle,
    source: &str,
    config: &AppConfig,
    results: &[ProcessingResult],
) {
    if results.is_empty() {
        write(
            app,
            "INFO",
            format!("source={source} mailbox_check new_messages=0"),
        );
        return;
    }

    for result in results {
        let (level, status, destination) = match result.status {
            ProcessingStatus::Uploaded => (
                "INFO",
                "uploaded",
                Some(config.processed_mailbox.as_str()),
            ),
            ProcessingStatus::ProcessedNoAttachments => (
                "INFO",
                "processed_no_attachments",
                Some(config.processed_mailbox.as_str()),
            ),
            ProcessingStatus::NeedsReview => (
                "WARN",
                "needs_review",
                Some(config.review_mailbox.as_str()),
            ),
            ProcessingStatus::Failed => ("ERROR", "failed", None),
        };

        let student = quoted(result.student_name.as_deref().unwrap_or("unknown"));
        let folder = quoted(result.folder_name.as_deref().unwrap_or("none"));
        let folder_id = quoted(result.folder_id.as_deref().unwrap_or("none"));
        let uploaded = quoted_list(&result.uploaded_file_names);
        let skipped = quoted_list(&result.skipped_existing_files);
        let moved_to = quoted(destination.unwrap_or("not_moved"));
        let detail = quoted(&result.detail);

        write(
            app,
            level,
            format!(
                "source={source} uid={} status={status} student={student} attachments={} folder={folder} folder_id={folder_id} uploaded={uploaded} skipped_existing={skipped} moved_to={moved_to} detail={detail}",
                result.uid, result.attachment_count
            ),
        );
    }

    write(
        app,
        "INFO",
        format!(
            "source={source} mailbox_check new_messages={} uploaded={} needs_review={} failed={}",
            results.len(),
            results
                .iter()
                .filter(|result| matches!(result.status, ProcessingStatus::Uploaded))
                .count(),
            results
                .iter()
                .filter(|result| matches!(result.status, ProcessingStatus::NeedsReview))
                .count(),
            results
                .iter()
                .filter(|result| matches!(result.status, ProcessingStatus::Failed))
                .count(),
        ),
    );
}

fn rotate_if_needed(path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() < MAX_LOG_BYTES {
        return;
    }
    let rotated = path.with_file_name(ROTATED_LOG_FILE);
    let _ = fs::remove_file(&rotated);
    let _ = fs::rename(path, rotated);
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn quoted_list(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".to_string();
    }
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| quoted(value))
            .collect::<Vec<_>>()
            .join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_values_are_single_line_and_escaped() {
        assert_eq!(quoted("a\\b\"c"), "\"a\\\\b\\\"c\"");
        assert_eq!(quoted_list(&["a.pdf".into(), "b.docx".into()]), "[\"a.pdf\",\"b.docx\"]");
    }
}
