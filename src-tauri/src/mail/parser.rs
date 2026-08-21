use std::path::Path;

use mailparse::{parse_mail, DispositionType, MailHeaderMap, ParsedMail};
use thiserror::Error;

use crate::models::{Attachment, ParsedMessage};

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid email: {0}")]
    InvalidEmail(String),
    #[error("unsafe attachment filename: {0}")]
    UnsafeFilename(String),
}

pub fn parse_message(raw: &[u8]) -> Result<ParsedMessage, ParseError> {
    let mail = parse_mail(raw).map_err(|e| ParseError::InvalidEmail(e.to_string()))?;
    let headers = &mail.headers;

    let mut parsed = ParsedMessage {
        subject: headers.get_first_value("Subject"),
        from: headers.get_first_value("From"),
        to: headers.get_first_value("To"),
        date: headers.get_first_value("Date"),
        message_id: headers.get_first_value("Message-ID"),
        ..Default::default()
    };

    collect_parts(&mail, &mut parsed)?;
    parsed.text_body = parsed.text_body.trim().to_string();
    parsed.html_body = parsed.html_body.trim().to_string();
    Ok(parsed)
}

fn collect_parts(part: &ParsedMail<'_>, output: &mut ParsedMessage) -> Result<(), ParseError> {
    if !part.subparts.is_empty() {
        for child in &part.subparts {
            collect_parts(child, output)?;
        }
        return Ok(());
    }

    let disposition = part.get_content_disposition();
    let filename = disposition
        .params
        .get("filename")
        .cloned()
        .or_else(|| part.ctype.params.get("name").cloned());

    let is_attachment =
        matches!(disposition.disposition, DispositionType::Attachment) || filename.is_some();

    if is_attachment {
        let filename = filename.unwrap_or_else(|| "attachment.bin".to_string());
        let safe = sanitize_filename(&filename)?;
        let bytes = part
            .get_body_raw()
            .map_err(|e| ParseError::InvalidEmail(e.to_string()))?;
        output.attachments.push(Attachment {
            filename: safe,
            content_type: part.ctype.mimetype.clone(),
            bytes,
        });
        return Ok(());
    }

    match part.ctype.mimetype.as_str() {
        "text/plain" => {
            let body = part
                .get_body()
                .map_err(|e| ParseError::InvalidEmail(e.to_string()))?;
            append_body(&mut output.text_body, &body);
        }
        "text/html" => {
            let body = part
                .get_body()
                .map_err(|e| ParseError::InvalidEmail(e.to_string()))?;
            append_body(&mut output.html_body, &body);
        }
        _ => {}
    }
    Ok(())
}

fn append_body(target: &mut String, value: &str) {
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(value);
}

fn sanitize_filename(filename: &str) -> Result<String, ParseError> {
    let trimmed = filename.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
    {
        return Err(ParseError::UnsafeFilename(filename.to_string()));
    }

    let path = Path::new(trimmed);
    if path.file_name().and_then(|p| p.to_str()) != Some(trimmed) {
        return Err(ParseError::UnsafeFilename(filename.to_string()));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multipart_body_and_attachment() {
        let raw = concat!(
            "From: admissions@example.edu\r\n",
            "To: agent@example.com\r\n",
            "Subject: Offer for Chang Rui\r\n",
            "Message-ID: <abc@example.edu>\r\n",
            "Content-Type: multipart/mixed; boundary=xyz\r\n",
            "\r\n",
            "--xyz\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Student Name: Chang Rui\r\nApplication ID: APP12345\r\n",
            "--xyz\r\n",
            "Content-Type: application/pdf\r\n",
            "Content-Disposition: attachment; filename=offer.pdf\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "SGVsbG8=\r\n",
            "--xyz--\r\n"
        );

        let parsed = parse_message(raw.as_bytes()).unwrap();
        assert_eq!(parsed.subject.as_deref(), Some("Offer for Chang Rui"));
        assert!(parsed.text_body.contains("Student Name: Chang Rui"));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename, "offer.pdf");
        assert_eq!(parsed.attachments[0].bytes, b"Hello");
    }

    #[test]
    fn rejects_path_traversal_filename() {
        assert!(sanitize_filename("../offer.pdf").is_err());
        assert!(sanitize_filename("..\\offer.pdf").is_err());
    }

    #[test]
    fn parses_unicode_attachment_filename() {
        let raw = concat!(
            "Content-Type: multipart/mixed; boundary=x\r\n\r\n",
            "--x\r\n",
            "Content-Type: application/pdf; name=录取通知.pdf\r\n",
            "Content-Disposition: attachment; filename=录取通知.pdf\r\n\r\n",
            "PDFDATA\r\n",
            "--x--\r\n"
        );
        let parsed = parse_message(raw.as_bytes()).unwrap();
        assert_eq!(parsed.attachments[0].filename, "录取通知.pdf");
    }
}
