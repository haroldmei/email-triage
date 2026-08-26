use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::{logging, models::ParsedMessage};

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
    attachments: Vec<MlAttachment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MlResponse {
    #[serde(default)]
    ocr_text: String,
    student_name: Option<String>,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    evidence: String,
}

fn is_image(filename: &str, content_type: &str) -> bool {
    let filename = filename.to_ascii_lowercase();
    let content_type = content_type.to_ascii_lowercase();
    content_type.starts_with("image/")
        || filename.ends_with(".png")
        || filename.ends_with(".jpg")
        || filename.ends_with(".jpeg")
        || filename.ends_with(".webp")
        || filename.ends_with(".bmp")
}

/// Enriches a parsed message with OCR/NER output from the local-only ML worker.
/// The worker is optional: if it is unavailable, the original message is returned unchanged.
pub async fn enrich_message(app: &AppHandle, uid: u32, message: &ParsedMessage) -> ParsedMessage {
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

    // NER can still help on unstructured email text, so call the worker even if there are no images.
    let request = MlRequest {
        subject: message.subject.clone(),
        text_body: message.text_body.clone(),
        html_body: message.html_body.clone(),
        attachments: image_attachments,
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            logging::write(
                app,
                "WARN",
                format!("stage=local_ml uid={uid} available=false reason=client_build_failed error=\"{}\"", error.to_string().replace('"', "'")),
            );
            return message.clone();
        }
    };

    let response = match client.post(LOCAL_ML_URL).json(&request).send().await {
        Ok(response) => response,
        Err(error) => {
            logging::write(
                app,
                "INFO",
                format!("stage=local_ml uid={uid} available=false fallback=deterministic reason=worker_unavailable error=\"{}\"", error.to_string().replace('"', "'")),
            );
            return message.clone();
        }
    };

    if !response.status().is_success() {
        logging::write(
            app,
            "WARN",
            format!("stage=local_ml uid={uid} available=true success=false http_status={} fallback=deterministic", response.status()),
        );
        return message.clone();
    }

    let result = match response.json::<MlResponse>().await {
        Ok(result) => result,
        Err(error) => {
            logging::write(
                app,
                "WARN",
                format!("stage=local_ml uid={uid} available=true success=false reason=invalid_response fallback=deterministic error=\"{}\"", error.to_string().replace('"', "'")),
            );
            return message.clone();
        }
    };

    let mut enriched = message.clone();
    if !result.ocr_text.trim().is_empty() {
        enriched.text_body.push_str("\n\n[LOCAL OCR]\n");
        enriched.text_body.push_str(result.ocr_text.trim());
    }

    // Only promote a machine-learned student name when the model is confident.
    // The deterministic resolver still validates plausibility and resolves Chinese-vs-English preference.
    if let Some(name) = result
        .student_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && result.confidence >= 0.85)
    {
        enriched.text_body.push_str("\nStudent Name: ");
        enriched.text_body.push_str(name);
        enriched.text_body.push('\n');
    }

    logging::write(
        app,
        "INFO",
        format!(
            "stage=local_ml uid={uid} available=true success=true ocr_chars={} student_name_found={} confidence={:.3} evidence=\"{}\"",
            result.ocr_text.chars().count(),
            result.student_name.is_some(),
            result.confidence,
            result.evidence.replace('"', "'")
        ),
    );

    enriched
}
