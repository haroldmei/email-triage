use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use tauri::{AppHandle, Manager};
use tokio::time::timeout;

use crate::{config, logging, workflow};

pub const PROCESSING_TIMEOUT: Duration = Duration::from_secs(600);

pub struct ProcessingState {
    active: AtomicBool,
    source: Mutex<Option<&'static str>>,
}

pub type ProcessingGate = Arc<ProcessingState>;

pub struct ProcessingLease {
    gate: ProcessingGate,
}

impl Drop for ProcessingLease {
    fn drop(&mut self) {
        self.gate.active.store(false, Ordering::Release);
        if let Ok(mut source) = self.gate.source.lock() {
            *source = None;
        }
    }
}

pub fn new_gate() -> ProcessingGate {
    Arc::new(ProcessingState {
        active: AtomicBool::new(false),
        source: Mutex::new(None),
    })
}

pub fn try_enter(gate: &ProcessingGate, source: &'static str) -> Option<ProcessingLease> {
    gate.active
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()?;
    if let Ok(mut current) = gate.source.lock() {
        *current = Some(source);
    }
    Some(ProcessingLease { gate: gate.clone() })
}

pub fn current_source(gate: &ProcessingGate) -> Option<String> {
    gate.source
        .lock()
        .ok()
        .and_then(|value| value.map(ToOwned::to_owned))
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let version = env!("CARGO_PKG_VERSION");
        loop {
            let wait = config::load(&app)
                .map(|cfg| cfg.poll_interval_seconds.max(30))
                .unwrap_or(60);
            tokio::time::sleep(Duration::from_secs(wait)).await;

            let Ok(cfg) = config::load(&app) else {
                logging::write(
                    &app,
                    "ERROR",
                    format!("version={version} source=background stage=configuration_load action=\"Loading saved configuration before scheduled mailbox check\" failed=true"),
                );
                continue;
            };
            if !is_ready(&cfg) {
                logging::write(
                    &app,
                    "INFO",
                    format!("version={version} source=background mailbox_check skipped reason=setup_incomplete action=\"Scheduled check skipped because mailbox, Google account, or Drive root is not fully configured\""),
                );
                continue;
            }

            let gate = app.state::<ProcessingGate>().inner().clone();
            let Some(_lease) = try_enter(&gate, "background") else {
                let active_source = current_source(&gate).unwrap_or_else(|| "unknown".into());
                logging::write(
                    &app,
                    "INFO",
                    format!("version={version} source=background mailbox_check skipped reason=processing_already_running active_source={active_source} action=\"Scheduled check skipped because another processing run is still active\""),
                );
                continue;
            };

            let mailbox = cfg
                .mail
                .as_ref()
                .map(|mail| mail.mailbox.as_str())
                .unwrap_or("unknown");
            let started = Instant::now();
            logging::write(
                &app,
                "INFO",
                format!(
                    "version={version} source=background mailbox_check mailbox=\"{mailbox}\" started poll_interval_seconds={} watchdog_seconds={} action=\"Checking Tencent mailbox, fetching candidate messages, extracting student identity, and filing attachments to Drive\"",
                    cfg.poll_interval_seconds,
                    PROCESSING_TIMEOUT.as_secs()
                ),
            );
            match timeout(PROCESSING_TIMEOUT, workflow::process_once(&app, &cfg, 100)).await {
                Ok(Ok(results)) => {
                    logging::write_processing_results(&app, "background", &results);
                    logging::write(
                        &app,
                        "INFO",
                        format!("version={version} source=background mailbox_check mailbox=\"{mailbox}\" completed elapsed_ms={} action=\"Scheduled mailbox check finished\"", started.elapsed().as_millis()),
                    );
                }
                Ok(Err(error)) => logging::write(
                    &app,
                    "ERROR",
                    format!("version={version} source=background mailbox_check mailbox=\"{mailbox}\" failed elapsed_ms={} action=\"Scheduled mailbox check stopped with an error\" error=\"{error}\"", started.elapsed().as_millis()),
                ),
                Err(_) => logging::write(
                    &app,
                    "ERROR",
                    format!("version={version} source=background mailbox_check mailbox=\"{mailbox}\" timed_out seconds={} elapsed_ms={} action=\"Scheduled mailbox check exceeded its overall watchdog timeout\"", PROCESSING_TIMEOUT.as_secs(), started.elapsed().as_millis()),
                ),
            }
        }
    });
}

fn is_ready(config: &crate::models::AppConfig) -> bool {
    config.mail.is_some()
        && config
            .google_client_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && config
            .google_email
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && config
            .drive_root_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_prevents_parallel_runs_and_reports_source() {
        let gate = new_gate();
        let lease = try_enter(&gate, "manual").expect("first run should enter");
        assert_eq!(current_source(&gate).as_deref(), Some("manual"));
        assert!(try_enter(&gate, "background").is_none());
        drop(lease);
        assert!(current_source(&gate).is_none());
        assert!(try_enter(&gate, "background").is_some());
    }

    #[test]
    fn processing_watchdog_allows_multiple_large_message_fetches() {
        assert_eq!(PROCESSING_TIMEOUT.as_secs(), 600);
        assert!(PROCESSING_TIMEOUT > crate::mail::MESSAGE_FETCH_TIMEOUT);
    }
}
