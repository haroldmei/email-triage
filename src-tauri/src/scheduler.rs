use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use tauri::{AppHandle, Manager};

use crate::{config, workflow};

pub type ProcessingGate = Arc<AtomicBool>;

pub fn new_gate() -> ProcessingGate {
    Arc::new(AtomicBool::new(false))
}

pub fn try_enter(gate: &ProcessingGate) -> bool {
    gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

pub fn leave(gate: &ProcessingGate) {
    gate.store(false, Ordering::Release);
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let wait = config::load(&app)
                .map(|cfg| cfg.poll_interval_seconds.max(30))
                .unwrap_or(60);
            tokio::time::sleep(Duration::from_secs(wait)).await;

            let Ok(cfg) = config::load(&app) else {
                continue;
            };
            if !is_ready(&cfg) {
                continue;
            }

            let gate = app.state::<ProcessingGate>().inner().clone();
            if !try_enter(&gate) {
                continue;
            }
            let result = workflow::process_once(&cfg, 100).await;
            leave(&gate);

            if let Err(error) = result {
                eprintln!("email-triage background run failed: {error}");
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
    fn gate_prevents_parallel_runs() {
        let gate = new_gate();
        assert!(try_enter(&gate));
        assert!(!try_enter(&gate));
        leave(&gate);
        assert!(try_enter(&gate));
    }
}
