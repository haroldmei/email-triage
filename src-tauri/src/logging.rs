use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use chrono::Local;
use tauri::{AppHandle, Manager};

use crate::models::{ProcessingResult, ProcessingStatus};

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

pub fn write_processing_results(app: &AppHandle, source: &str, results: &[ProcessingResult]) {
    let version = env!("CARGO_PKG_VERSION");
    if results.is_empty() {
        write(
            app,
            "INFO",
            format!(
                "version={version} source={source} stage=batch_summary action=\"Mailbox check finished; no candidate messages required processing\" candidate_messages=0 uploaded=0 processed_no_attachments=0 needs_review=0 failed=0 attachments_found=0 files_uploaded=0 skipped_existing=0"
            ),
        );
        return;
    }

    for result in results {
        let (level, status, local_state, explanation) = match result.status {
            ProcessingStatus::Uploaded => (
                "INFO",
                "uploaded",
                "completed",
                "Attachments were filed in Google Drive",
            ),
            ProcessingStatus::ProcessedNoAttachments => (
                "INFO",
                "processed_no_attachments",
                "completed",
                "Message contained no attachments; no Drive upload was needed",
            ),
            ProcessingStatus::NeedsReview => (
                "WARN",
                "needs_review",
                "needs_review",
                "Automatic filing stopped because a safe student identity or folder decision was unavailable",
            ),
            ProcessingStatus::Failed => (
                "ERROR",
                "failed",
                "retryable",
                "Processing failed and should be retried",
            ),
        };

        let student = quoted(result.student_name.as_deref().unwrap_or("unknown"));
        let folder = quoted(result.folder_name.as_deref().unwrap_or("none"));
        let folder_id = quoted(result.folder_id.as_deref().unwrap_or("none"));
        let uploaded = quoted_list(&result.uploaded_file_names);
        let skipped = quoted_list(&result.skipped_existing_files);
        let detail = quoted(&result.detail);
        let explanation = quoted(explanation);

        write(
            app,
            level,
            format!(
                "version={version} source={source} stage=result_summary action=\"Message processing result\" uid={} status={status} student={student} attachments_found={} drive_folder={folder} drive_folder_id={folder_id} files_uploaded_count={} files_uploaded={uploaded} skipped_existing_count={} skipped_existing={skipped} local_state={local_state} mail_server_mutated=false explanation={explanation} detail={detail}",
                result.uid,
                result.attachment_count,
                result.uploaded_file_names.len(),
                result.skipped_existing_files.len()
            ),
        );
    }

    let uploaded = results
        .iter()
        .filter(|result| matches!(result.status, ProcessingStatus::Uploaded))
        .count();
    let no_attachments = results
        .iter()
        .filter(|result| matches!(result.status, ProcessingStatus::ProcessedNoAttachments))
        .count();
    let needs_review = results
        .iter()
        .filter(|result| matches!(result.status, ProcessingStatus::NeedsReview))
        .count();
    let failed = results
        .iter()
        .filter(|result| matches!(result.status, ProcessingStatus::Failed))
        .count();
    let attachments_found: usize = results.iter().map(|result| result.attachment_count).sum();
    let files_uploaded: usize = results
        .iter()
        .map(|result| result.uploaded_file_names.len())
        .sum();
    let skipped_existing: usize = results
        .iter()
        .map(|result| result.skipped_existing_files.len())
        .sum();

    write(
        app,
        "INFO",
        format!(
            "version={version} source={source} stage=batch_summary action=\"Mailbox check processing summary\" candidate_messages={} uploaded={} processed_no_attachments={} needs_review={} failed={} attachments_found={} files_uploaded={} skipped_existing={}",
            results.len(),
            uploaded,
            no_attachments,
            needs_review,
            failed,
            attachments_found,
            files_uploaded,
            skipped_existing
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
        assert_eq!(
            quoted_list(&["a.pdf".into(), "b.docx".into()]),
            "[\"a.pdf\",\"b.docx\"]"
        );
    }
}
