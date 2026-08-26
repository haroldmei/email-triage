use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::{extraction, logging, models::ParsedMessage};

const LOCAL_ML_URL: &str = "http://127.0.0.1:8765/extract";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MlAttachment {
    filename: String,
    content_type: String,
    data_base64: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MlRequest {
    subject: Option<String>,
    text_body: String,
    html_body: String,
    document_text: String,
    attachments: Vec<MlAttachment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MlResponse {
    #[serde(default)]
    ocr_text: String,
    #[serde(default)]
    ocr_errors: Vec<String>,
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
        evidence: "local ML unavailable".into(),
    }
}

pub async fn enrich_message(app: &AppHandle, uid: u32, message: &ParsedMessage) -> MlEnrichment {
    let image_attachments = message
        .attachments
        .iter()
        .filter(|attachment| is_image(&attachment.filename, &attachment.content_type))
        .map(|attachment| MlAttachment {
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            data_base64: STANDARD.encode(&attachment.bytes),
        })
        .collect::<Vec<_>>();

    let mut document_parts = Vec::new();
    for attachment in &message.attachments {
        if let Some(text) = extraction::extract_attachment_text(attachment) {
            if !text.trim().is_empty() {
                document_parts.push(format!("[ATTACHMENT {}]\n{}", attachment.filename, text));
            }
        } else {
            document_parts.push(format!("[ATTACHMENT {}]", attachment.filename));
        }
    }

    let request = MlRequest {
        subject: message.subject.clone(),
        text_body: message.text_body.clone(),
        html_body: message.html_body.clone(),
        document_text: document_parts.join("\n"),
        attachments: image_attachments,
    };

    let client = match reqwest::Client::builder().timeout(Duration::from_secs(60)).build() {
        Ok(client) => client,
        Err(error) => {
            logging::write(app, "WARN", format!("stage=local_ml uid={uid} available=false reason=client_build_failed error=\"{}\"", error.to_string().replace('"', "'")));
            return fallback(message);
        }
    };

    let response = match client.post(LOCAL_ML_URL).json(&request).send().await {
        Ok(response) => response,
        Err(error) => {
            logging::write(app, "INFO", format!("stage=local_ml uid={uid} available=false fallback=deterministic reason=worker_unavailable error=\"{}\"", error.to_string().replace('"', "'")));
            return fallback(message);
        }
    };

    if !response.status().is_success() {
        logging::write(app, "WARN", format!("stage=local_ml uid={uid} available=true success=false http_status={} fallback=deterministic", response.status()));
        return fallback(message);
    }

    let result = match response.json::<MlResponse>().await {
        Ok(result) => result,
        Err(error) => {
            logging::write(app, "WARN", format!("stage=local_ml uid={uid} available=true success=false reason=invalid_response fallback=deterministic error=\"{}\"", error.to_string().replace('"', "'")));
            return fallback(message);
        }
    };

    let mut enriched = message.clone();
    if !result.ocr_text.trim().is_empty() {
        enriched.text_body.push_str("\n\n[LOCAL OCR]\n");
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
            "stage=local_ml uid={uid} available=true success=true ocr_chars={} ocr_errors={} student_name_found={} accepted={} confidence={:.3} candidate=\"{}\" evidence=\"{}\"",
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
    }
}
