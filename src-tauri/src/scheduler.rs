use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use tauri::{AppHandle, Manager};
use tokio::time::timeout;

use crate::{config, logging, workflow};

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
        loop {
            let wait = config::load(&app)
                .map(|cfg| cfg.poll_interval_seconds.max(30))
                .unwrap_or(60);
            tokio::time::sleep(Duration::from_secs(wait)).await;

            let Ok(cfg) = config::load(&app) else {
                logging::write(&app, "ERROR", "source=background configuration_load failed");
                continue;
            };
            if !is_ready(&cfg) {
                logging::write(&app, "INFO", "source=background mailbox_check skipped reason=setup_incomplete");
                continue;
            }

            let gate = app.state::<ProcessingGate>().inner().clone();
            let Some(_lease) = try_enter(&gate, "background") else {
                logging::write(&app, "INFO", "source=background mailbox_check skipped reason=processing_already_running");
                continue;
            };

            logging::write(
                &app,
                "INFO",
                format!(
                    "source=background mailbox_check started poll_interval_seconds={}",
                    cfg.poll_interval_seconds
                ),
            );
            match timeout(Duration::from_secs(120), workflow::process_once(&cfg, 100)).await {
                Ok(Ok(results)) => {
                    logging::write_processing_results(&app, "background", &cfg, &results)
                }
                Ok(Err(error)) => logging::write(
                    &app,
                    "ERROR",
                    format!("source=background mailbox_check failed error=\"{error}\""),
                ),
                Err(_) => logging::write(
                    &app,
                    "ERROR",
                    "source=background mailbox_check timed_out seconds=120",
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
}
