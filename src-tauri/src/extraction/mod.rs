use regex::Regex;

use crate::models::{ExtractedValue, ParsedMessage, StudentIdentity};

pub trait IdentityExtractor {
    fn extract(&self, message: &ParsedMessage) -> StudentIdentity;
}

#[derive(Default)]
pub struct DeterministicExtractor;

impl IdentityExtractor for DeterministicExtractor {
    fn extract(&self, message: &ParsedMessage) -> StudentIdentity {
        let body = searchable_text(message);
        StudentIdentity {
            name: capture(
                &body,
                r"(?im)^\s*(?:student\s*name|applicant\s*name|学生姓名|姓名)\s*[:：]\s*([^\r\n]{2,80})\s*$",
                "labeled student name",
                0.99,
            ),
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
    }
    parts.join("\n")
}

fn strip_html(html: &str) -> String {
    let tag_re = Regex::new(r"(?is)<[^>]+>").expect("constant HTML tag regex");
    tag_re.replace_all(html, "\n").into_owned()
}

fn capture(text: &str, pattern: &str, evidence: &str, confidence: f32) -> Option<ExtractedValue> {
    let re = Regex::new(pattern).expect("constant extraction regex");
    let captures = re.captures(text)?;
    let value = captures.get(1)?.as_str().trim().trim_matches(['\"', '\'']).to_string();
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
    use crate::models::ParsedMessage;

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
        assert_eq!(identity.application_id.unwrap().value, "APP12345");
        assert_eq!(identity.date_of_birth.unwrap().value, "2001-05-17");
        assert_eq!(identity.university.unwrap().value, "Example University");
        assert_eq!(identity.course.unwrap().value, "Master of Data Science");
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
        assert_eq!(identity.application_id.unwrap().value, "A9988");
    }
}
