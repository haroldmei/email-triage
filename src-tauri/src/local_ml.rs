use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::{extraction, logging, models::ParsedMessage};

const LOCAL_ML_URL: &str = "http://127.0.0.1:8765/extract";
const LOCAL_ML_CONNECT_TIMEOUT_SECS: u64 = 5;
const LOCAL_ML_REQUEST_TIMEOUT_SECS: u64 = 600;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MlAttachment {
    filename: String,
    content_type: String,
    data_base64: String,
    extracted_text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MlRequest {
    subject: Option<String>,
    text_body: String,
    html_body: String,
    attachments: Vec<MlAttachment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentDecision {
    filename: String,
    relevant: bool,
    #[serde(default)]
    category: String,
    #[serde(default)]
    score: f32,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MlResponse {
    #[serde(default)]
    ocr_text: String,
    #[serde(default)]
    ocr_errors: Vec<String>,
    #[serde(default)]
    attachment_decisions: Vec<AttachmentDecision>,
    student_name: Option<String>,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    evidence: String,
}

#[derive(Debug, Clone)]
pub struct MlEnrichment {
    pub message: ParsedMessage,
    pub student_name: Option<String>,
    pub confidence: f32,
    pub evidence: String,
    pub classification_available: bool,
    pub relevant_filenames: Vec<String>,
}

fn is_image(filename: &str, content_type: &str) -> bool {
    let filename = filename.to_ascii_lowercase();
    let content_type = content_type.to_ascii_lowercase();
    content_type.starts_with("image/")
        || filename.ends_with(".png")
        || filename.ends_with(".jpg")
        || filename.ends_with(".jpeg")
        || filename.ends_with(".gif")
        || filename.ends_with(".webp")
        || filename.ends_with(".bmp")
}

fn fallback(message: &ParsedMessage) -> MlEnrichment {
    MlEnrichment {
        message: message.clone(),
        student_name: None,
        confidence: 0.0,
        evidence: "local application-material classifier unavailable".into(),
        classification_available: false,
        relevant_filenames: Vec::new(),
    }
}

pub async fn enrich_message(app: &AppHandle, uid: u32, message: &ParsedMessage) -> MlEnrichment {
    let attachments = message
        .attachments
        .iter()
        .map(|attachment| MlAttachment {
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            data_base64: if is_image(&attachment.filename, &attachment.content_type) {
                STANDARD.encode(&attachment.bytes)
            } else {
                String::new()
            },
            extracted_text: extraction::extract_attachment_text(attachment).unwrap_or_default(),
        })
        .collect::<Vec<_>>();

    let request = MlRequest {
        subject: message.subject.clone(),
        text_body: message.text_body.clone(),
        html_body: message.html_body.clone(),
        attachments,
    };

    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(LOCAL_ML_CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(LOCAL_ML_REQUEST_TIMEOUT_SECS))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            logging::write(app, "WARN", format!("stage=local_ml uid={uid} available=false reason=client_build_failed error=\"{}\"", error.to_string().replace('"', "'")));
            return fallback(message);
        }
    };

    let request_started = Instant::now();
    logging::write(
        app,
        "INFO",
        format!(
            "stage=local_ml_request uid={uid} action=start attachments={} timeout_secs={LOCAL_ML_REQUEST_TIMEOUT_SECS}",
            message.attachments.len()
        ),
    );

    let response = match client.post(LOCAL_ML_URL).json(&request).send().await {
        Ok(response) => response,
        Err(error) => {
            logging::write(
                app,
                "WARN",
                format!(
                    "stage=local_ml_request uid={uid} action=failed elapsed_ms={} error=\"{}\"",
                    request_started.elapsed().as_millis(),
                    error.to_string().replace('"', "'")
                ),
            );
            return fallback(message);
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        logging::write(
            app,
            "WARN",
            format!(
                "stage=local_ml_request uid={uid} action=http_error elapsed_ms={} http_status={} body=\"{}\"",
                request_started.elapsed().as_millis(),
                status,
                body.chars().take(500).collect::<String>().replace('"', "'")
            ),
        );
        return fallback(message);
    }

    let result = match response.json::<MlResponse>().await {
        Ok(result) => result,
        Err(error) => {
            logging::write(
                app,
                "WARN",
                format!(
                    "stage=local_ml_request uid={uid} action=invalid_response elapsed_ms={} error=\"{}\"",
                    request_started.elapsed().as_millis(),
                    error.to_string().replace('"', "'")
                ),
            );
            return fallback(message);
        }
    };

    logging::write(
        app,
        "INFO",
        format!(
            "stage=local_ml_request uid={uid} action=complete elapsed_ms={}",
            request_started.elapsed().as_millis()
        ),
    );

    let relevant_filenames = result
        .attachment_decisions
        .iter()
        .filter(|decision| decision.relevant)
        .map(|decision| decision.filename.clone())
        .collect::<Vec<_>>();

    for decision in &result.attachment_decisions {
        logging::write(
            app,
            "INFO",
            format!(
                "stage=application_material uid={uid} filename=\"{}\" relevant={} category=\"{}\" score={:.3} reason=\"{}\"",
                decision.filename.replace('"', "'"),
                decision.relevant,
                decision.category.replace('"', "'"),
                decision.score,
                decision.reason.replace('"', "'")
            ),
        );
    }

    let mut enriched = message.clone();
    enriched.attachments.retain(|attachment| {
        relevant_filenames.iter().any(|filename| filename == &attachment.filename)
    });
    if !result.ocr_text.trim().is_empty() {
        enriched.text_body.push_str("\n\n[LOCAL OCR FROM APPLICATION MATERIALS]\n");
        enriched.text_body.push_str(result.ocr_text.trim());
    }

    let accepted_name = result
        .student_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && result.confidence >= 0.85)
        .map(str::to_string);

    logging::write(
        app,
        "INFO",
        format!(
            "stage=local_ml uid={uid} available=true success=true attachments={} relevant_attachments={} ocr_chars={} ocr_errors={} student_name_found={} accepted={} confidence={:.3} candidate=\"{}\" evidence=\"{}\"",
            message.attachments.len(),
            relevant_filenames.len(),
            result.ocr_text.chars().count(),
            result.ocr_errors.len(),
            result.student_name.is_some(),
            accepted_name.is_some(),
            result.confidence,
            result.student_name.as_deref().unwrap_or("none").replace('"', "'"),
            result.evidence.replace('"', "'")
        ),
    );

    for error in result.ocr_errors.iter().take(5) {
        logging::write(app, "WARN", format!("stage=local_ml_ocr uid={uid} failed=true error=\"{}\"", error.replace('"', "'")));
    }

    MlEnrichment {
        message: enriched,
        student_name: accepted_name,
        confidence: result.confidence,
        evidence: result.evidence,
        classification_available: true,
        relevant_filenames,
    }
}
