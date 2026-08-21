use std::io::{Cursor, Read};

use regex::Regex;

use crate::models::{Attachment, ExtractedValue, ParsedMessage, StudentIdentity};

pub trait IdentityExtractor {
    fn extract(&self, message: &ParsedMessage) -> StudentIdentity;
}

#[derive(Default)]
pub struct DeterministicExtractor;

impl IdentityExtractor for DeterministicExtractor {
    fn extract(&self, message: &ParsedMessage) -> StudentIdentity {
        let body = searchable_text(message);
        let generic_name = capture(
            &body,
            r"(?im)^\s*(?:student\s*name|applicant\s*name|学生姓名|姓名)\s*[:：]\s*([^\r\n]{2,120})\s*$",
            "labeled student name",
            0.99,
        );
        let mut english_name = capture(
            &body,
            r"(?im)^\s*(?:english\s*name|name\s*\(\s*english\s*\)|英文名|英文姓名)\s*[:：]\s*([^\r\n]{2,80})\s*$",
            "labeled English student name",
            0.99,
        );
        let mut chinese_name = capture(
            &body,
            r"(?im)^\s*(?:chinese\s*name|name\s*\(\s*chinese\s*\)|中文名|中文姓名)\s*[:：]\s*([^\r\n]{2,40})\s*$",
            "labeled Chinese student name",
            0.99,
        );

        if let Some(generic) = &generic_name {
            let (generic_english, generic_chinese) = split_bilingual_name(&generic.value);
            if english_name.is_none() {
                english_name = generic_english.map(|value| ExtractedValue {
                    value,
                    confidence: generic.confidence,
                    evidence: format!("{} (English component)", generic.evidence),
                });
            }
            if chinese_name.is_none() {
                chinese_name = generic_chinese.map(|value| ExtractedValue {
                    value,
                    confidence: generic.confidence,
                    evidence: format!("{} (Chinese component)", generic.evidence),
                });
            }
        }

        let name = english_name
            .clone()
            .or_else(|| chinese_name.clone())
            .or(generic_name);

        StudentIdentity {
            name,
            english_name,
            chinese_name,
            application_id: capture(
                &body,
                r"(?im)^\s*(?:student\s*(?:id|number)|application\s*(?:id|number|no\.?|ref(?:erence)?)|学号|申请号)\s*[:：#]?\s*([A-Z0-9][A-Z0-9._/-]{2,40})\s*$",
                "labeled student/application identifier",
                0.99,
            ),
            date_of_birth: capture(
                &body,
                r"(?im)^\s*(?:date\s*of\s*birth|dob|出生日期)\s*[:：]\s*([0-9]{1,4}[./-][0-9]{1,2}[./-][0-9]{1,4})\s*$",
                "labeled date of birth",
                0.98,
            ),
            university: capture(
                &body,
                r"(?im)^\s*(?:university|institution|school|院校|大学)\s*[:：]\s*([^\r\n]{2,120})\s*$",
                "labeled university",
                0.95,
            ),
            course: capture(
                &body,
                r"(?im)^\s*(?:course|program(?:me)?|degree|专业|课程)\s*[:：]\s*([^\r\n]{2,120})\s*$",
                "labeled course/program",
                0.94,
            ),
        }
    }
}

fn searchable_text(message: &ParsedMessage) -> String {
    let mut parts = Vec::new();
    if let Some(subject) = &message.subject {
        parts.push(subject.clone());
    }
    if !message.text_body.is_empty() {
        parts.push(message.text_body.clone());
    }
    if !message.html_body.is_empty() {
        parts.push(strip_html(&message.html_body));
    }
    for attachment in &message.attachments {
        parts.push(format!("Attachment filename: {}", attachment.filename));
        if let Some(text) = extract_attachment_text(attachment) {
            parts.push(format!("Attachment content ({}):\n{}", attachment.filename, text));
        }
    }
    parts.join("\n")
}

fn extract_attachment_text(attachment: &Attachment) -> Option<String> {
    const MAX_TEXT_BYTES: usize = 2_000_000;
    let content_type = attachment.content_type.to_ascii_lowercase();
    let filename = attachment.filename.to_ascii_lowercase();

    if content_type.starts_with("text/")
        || filename.ends_with(".txt")
        || filename.ends_with(".csv")
        || filename.ends_with(".json")
        || filename.ends_with(".xml")
        || filename.ends_with(".html")
        || filename.ends_with(".htm")
    {
        let bytes = &attachment.bytes[..attachment.bytes.len().min(MAX_TEXT_BYTES)];
        let text = String::from_utf8_lossy(bytes).into_owned();
        return Some(if content_type.contains("html") || filename.ends_with(".html") || filename.ends_with(".htm") {
            strip_html(&text)
        } else {
            text
        });
    }

    if content_type == "application/pdf" || filename.ends_with(".pdf") {
        return pdf_extract::extract_text_from_mem(&attachment.bytes)
            .ok()
            .filter(|text| !text.trim().is_empty());
    }

    if content_type
        == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        || filename.ends_with(".docx")
    {
        return extract_docx_text(&attachment.bytes);
    }

    None
}

fn extract_docx_text(bytes: &[u8]) -> Option<String> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;
    let mut document = archive.by_name("word/document.xml").ok()?;
    let mut xml = String::new();
    document.read_to_string(&mut xml).ok()?;
    let text = strip_xml(&xml);
    (!text.trim().is_empty()).then_some(text)
}

fn strip_html(html: &str) -> String {
    let tag_re = Regex::new(r"(?is)<[^>]+>").expect("constant HTML tag regex");
    decode_xml_entities(&tag_re.replace_all(html, "\n"))
}

fn strip_xml(xml: &str) -> String {
    let paragraph_re = Regex::new(r"(?is)</w:(?:p|tr|tc)>").expect("constant Word paragraph regex");
    let tag_re = Regex::new(r"(?is)<[^>]+>").expect("constant XML tag regex");
    let with_breaks = paragraph_re.replace_all(xml, "\n");
    decode_xml_entities(&tag_re.replace_all(&with_breaks, ""))
}

fn decode_xml_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn split_bilingual_name(value: &str) -> (Option<String>, Option<String>) {
    let han_re = Regex::new(r"[\p{Han}]{2,8}").expect("constant Han name regex");
    let latin_re = Regex::new(r"[A-Za-z][A-Za-z .'-]{1,79}").expect("constant Latin name regex");

    let chinese = han_re
        .find(value)
        .map(|m| m.as_str().trim().to_string())
        .filter(|value| !value.is_empty());
    let english = latin_re
        .find(value)
        .map(|m| m.as_str().trim_matches(|ch: char| ch.is_whitespace() || "/|,;()（）".contains(ch)).to_string())
        .filter(|value| value.chars().any(|ch| ch.is_alphabetic()) && !value.is_empty());
    (english, chinese)
}

fn capture(text: &str, pattern: &str, evidence: &str, confidence: f32) -> Option<ExtractedValue> {
    let re = Regex::new(pattern).expect("constant extraction regex");
    let captures = re.captures(text)?;
    let value = captures
        .get(1)?
        .as_str()
        .trim()
        .trim_matches(['\"', '\''])
        .to_string();
    if value.is_empty() {
        return None;
    }
    Some(ExtractedValue {
        value,
        confidence,
        evidence: evidence.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Attachment, ParsedMessage};

    #[test]
    fn extracts_labeled_identity_fields() {
        let message = ParsedMessage {
            subject: Some("Application update".into()),
            text_body: concat!(
                "Student Name: Chang Rui\n",
                "Application ID: APP12345\n",
                "DOB: 2001-05-17\n",
                "University: Example University\n",
                "Course: Master of Data Science\n"
            )
            .into(),
            ..Default::default()
        };

        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.unwrap().value, "Chang Rui");
        assert_eq!(identity.english_name.unwrap().value, "Chang Rui");
        assert_eq!(identity.application_id.unwrap().value, "APP12345");
        assert_eq!(identity.date_of_birth.unwrap().value, "2001-05-17");
        assert_eq!(identity.university.unwrap().value, "Example University");
        assert_eq!(identity.course.unwrap().value, "Master of Data Science");
    }

    #[test]
    fn extracts_bilingual_names_from_message() {
        let message = ParsedMessage {
            text_body: "Student Name: 常瑞 / Chang Rui".into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.chinese_name.unwrap().value, "常瑞");
        assert_eq!(identity.english_name.unwrap().value, "Chang Rui");
    }

    #[test]
    fn extracts_bilingual_names_from_text_attachment() {
        let message = ParsedMessage {
            text_body: "Please see attached application.".into(),
            attachments: vec![Attachment {
                filename: "application.txt".into(),
                content_type: "text/plain".into(),
                bytes: "中文姓名: 常瑞\nEnglish Name: Chang Rui\n申请号: APP9988"
                    .as_bytes()
                    .to_vec(),
            }],
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.chinese_name.unwrap().value, "常瑞");
        assert_eq!(identity.english_name.unwrap().value, "Chang Rui");
        assert_eq!(identity.application_id.unwrap().value, "APP9988");
    }

    #[test]
    fn does_not_guess_an_unlabeled_name() {
        let message = ParsedMessage {
            subject: Some("Offer for Chang Rui".into()),
            text_body: "Please see attached offer.".into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert!(identity.name.is_none());
    }

    #[test]
    fn extracts_from_html_only_message() {
        let message = ParsedMessage {
            html_body: "<p>Student Name: 常瑞</p><p>Application ID: A9988</p>".into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.unwrap().value, "常瑞");
        assert_eq!(identity.chinese_name.unwrap().value, "常瑞");
        assert_eq!(identity.application_id.unwrap().value, "A9988");
    }
}
