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

        let generic_name = capture_first(
            &body,
            &[
                r"(?im)^[ \t]*(?:student\s*name|applicant\s*name|name\s*of\s*(?:student|applicant)|full\s*name|student|applicant|学生姓名|申请学生姓名|申请人姓名|申请人|学生|姓名)[ \t]*(?:[:：=\-])[ \t]*([^\r\n]{2,120})[ \t]*$",
                r"(?im)^[ \t]*(?:student\s*name|applicant\s*name|name\s*of\s*(?:student|applicant)|full\s*name|学生姓名|申请学生姓名|申请人姓名|姓名)[ \t]{2,}([^\r\n]{2,120})[ \t]*$",
                r"(?im)^[ \t]*(?:student\s*name|applicant\s*name|name\s*of\s*(?:student|applicant)|full\s*name|学生姓名|申请学生姓名|申请人姓名|姓名)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,120})[ \t]*$",
            ],
            "labeled student name",
            0.99,
        )
        .or_else(|| name_from_attachment_filename(message));

        let mut english_name = capture_first(
            &body,
            &[
                r"(?im)^[ \t]*(?:english\s*(?:full\s*)?name|name\s*\(\s*english\s*\)|name\s*in\s*english|英文名|英文姓名)[ \t]*(?:[:：=\-])[ \t]*([^\r\n]{2,80})[ \t]*$",
                r"(?im)^[ \t]*(?:english\s*(?:full\s*)?name|name\s*\(\s*english\s*\)|name\s*in\s*english|英文名|英文姓名)[ \t]{2,}([^\r\n]{2,80})[ \t]*$",
                r"(?im)^[ \t]*(?:english\s*(?:full\s*)?name|name\s*\(\s*english\s*\)|name\s*in\s*english|英文名|英文姓名)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,80})[ \t]*$",
            ],
            "labeled English student name",
            0.99,
        );

        let mut chinese_name = capture_first(
            &body,
            &[
                r"(?im)^[ \t]*(?:chinese\s*(?:full\s*)?name|name\s*\(\s*chinese\s*\)|name\s*in\s*chinese|中文名|中文姓名|姓名\s*\(\s*中文\s*\)|姓名（中文）)[ \t]*(?:[:：=\-])[ \t]*([^\r\n]{2,40})[ \t]*$",
                r"(?im)^[ \t]*(?:chinese\s*(?:full\s*)?name|name\s*\(\s*chinese\s*\)|name\s*in\s*chinese|中文名|中文姓名|姓名\s*\(\s*中文\s*\)|姓名（中文）)[ \t]{2,}([^\r\n]{2,40})[ \t]*$",
                r"(?im)^[ \t]*(?:chinese\s*(?:full\s*)?name|name\s*\(\s*chinese\s*\)|name\s*in\s*chinese|中文名|中文姓名|姓名\s*\(\s*中文\s*\)|姓名（中文）)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,40})[ \t]*$",
            ],
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

        // Chinese is the canonical student name. English is only a fallback.
        let name = chinese_name
            .clone()
            .or_else(|| english_name.clone())
            .or(generic_name);

        StudentIdentity {
            name,
            english_name,
            chinese_name,
            application_id: capture_first(
                &body,
                &[
                    r"(?im)^[ \t]*(?:student\s*(?:id|number|no\.?|reference)|application\s*(?:id|number|no\.?|ref(?:erence)?)|学号|学生编号|申请号|申请编号|申请ID)[ \t]*[:：#=\-]?[ \t]*([A-Z0-9][A-Z0-9._/-]{2,40})[ \t]*$",
                    r"(?im)^[ \t]*(?:student\s*(?:id|number|no\.?|reference)|application\s*(?:id|number|no\.?|ref(?:erence)?)|学号|学生编号|申请号|申请编号|申请ID)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([A-Z0-9][A-Z0-9._/-]{2,40})[ \t]*$",
                ],
                "labeled student/application identifier",
                0.99,
            ),
            date_of_birth: capture_first(
                &body,
                &[
                    r"(?im)^[ \t]*(?:date\s*of\s*birth|birth\s*date|dob|出生日期|生日)[ \t]*[:：=\-][ \t]*([0-9]{1,4}[./-][0-9]{1,2}[./-][0-9]{1,4})[ \t]*$",
                    r"(?im)^[ \t]*(?:date\s*of\s*birth|birth\s*date|dob|出生日期|生日)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([0-9]{1,4}[./-][0-9]{1,2}[./-][0-9]{1,4})[ \t]*$",
                ],
                "labeled date of birth",
                0.98,
            ),
            university: capture_first(
                &body,
                &[
                    r"(?im)^[ \t]*(?:university|institution|school|院校|学校|大学)[ \t]*[:：=\-][ \t]*([^\r\n]{2,120})[ \t]*$",
                    r"(?im)^[ \t]*(?:university|institution|school|院校|学校|大学)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,120})[ \t]*$",
                ],
                "labeled university",
                0.95,
            ),
            course: capture_first(
                &body,
                &[
                    r"(?im)^[ \t]*(?:course|program(?:me)?|degree|major|专业|课程|项目)[ \t]*[:：=\-][ \t]*([^\r\n]{2,120})[ \t]*$",
                    r"(?im)^[ \t]*(?:course|program(?:me)?|degree|major|专业|课程|项目)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,120})[ \t]*$",
                ],
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

fn name_from_attachment_filename(message: &ParsedMessage) -> Option<ExtractedValue> {
    let labeled = Regex::new(
        r"(?i)(?:student[-_ ]*name|applicant[-_ ]*name|name|学生姓名|申请人姓名|姓名)[-_ ：:=]+([\p{Han}]{2,4}|[A-Za-z][A-Za-z .'-]{1,60})",
    )
    .expect("constant attachment filename name regex");
    let chinese_prefix = Regex::new(
        r"^([\p{Han}]{2,4})[-_ ]+(?:护照|成绩单|申请|申请材料|简历|offer|cv|resume|passport|transcript)",
    )
    .expect("constant Chinese attachment filename regex");
    let english_prefix = Regex::new(
        r"(?i)^([A-Za-z][A-Za-z .'-]{2,60})[-_ ]+(?:offer|cv|resume|passport|transcript|application)",
    )
    .expect("constant English attachment filename regex");

    for attachment in &message.attachments {
        let stem = attachment
            .filename
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(&attachment.filename);
        for re in [&labeled, &chinese_prefix, &english_prefix] {
            if let Some(captures) = re.captures(stem) {
                let Some(value) = captures.get(1) else {
                    continue;
                };
                let value = clean_name_candidate(value.as_str());
                if is_plausible_name(&value) {
                    return Some(ExtractedValue {
                        value,
                        confidence: 0.95,
                        evidence: format!("attachment filename: {}", attachment.filename),
                    });
                }
            }
        }
    }
    None
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
        .map(|m| clean_name_candidate(m.as_str()))
        .filter(|value| is_plausible_name(value));
    let english = latin_re
        .find(value)
        .map(|m| clean_name_candidate(m.as_str()))
        .filter(|value| is_plausible_name(value));
    (english, chinese)
}

fn clean_name_candidate(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| ch.is_whitespace() || "/|,;()（）[]{}:：".contains(ch))
        .to_string()
}

fn is_plausible_name(value: &str) -> bool {
    let value = value.trim();
    if value.len() < 2 || value.len() > 120 {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    let blocked = [
        "application",
        "application form",
        "passport",
        "transcript",
        "resume",
        "curriculum vitae",
        "申请材料",
        "申请表",
        "成绩单",
        "护照",
    ];
    if blocked.iter().any(|item| lower == *item) {
        return false;
    }
    value.chars().any(|ch| ch.is_alphabetic())
}

fn capture_first(
    text: &str,
    patterns: &[&str],
    evidence: &str,
    confidence: f32,
) -> Option<ExtractedValue> {
    patterns
        .iter()
        .find_map(|pattern| capture(text, pattern, evidence, confidence))
}

fn capture(text: &str, pattern: &str, evidence: &str, confidence: f32) -> Option<ExtractedValue> {
    let re = Regex::new(pattern).expect("constant extraction regex");
    let captures = re.captures(text)?;
    let value = clean_name_candidate(captures.get(1)?.as_str());
    if !is_plausible_name(&value) {
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
    fn prefers_chinese_name_when_both_are_present() {
        let message = ParsedMessage {
            text_body: "Student Name: 常瑞 / Chang Rui".into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.as_ref().unwrap().value, "常瑞");
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
        assert_eq!(identity.name.as_ref().unwrap().value, "常瑞");
        assert_eq!(identity.chinese_name.unwrap().value, "常瑞");
        assert_eq!(identity.english_name.unwrap().value, "Chang Rui");
        assert_eq!(identity.application_id.unwrap().value, "APP9988");
    }

    #[test]
    fn extracts_name_from_adjacent_table_lines() {
        let message = ParsedMessage {
            text_body: "申请人姓名\n张伟\nName in English\nWei Zhang".into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.as_ref().unwrap().value, "张伟");
        assert_eq!(identity.chinese_name.unwrap().value, "张伟");
        assert_eq!(identity.english_name.unwrap().value, "Wei Zhang");
    }

    #[test]
    fn extracts_name_from_common_filename_pattern() {
        let message = ParsedMessage {
            attachments: vec![Attachment {
                filename: "张伟_护照.pdf".into(),
                content_type: "application/pdf".into(),
                bytes: Vec::new(),
            }],
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.as_ref().unwrap().value, "张伟");
        assert_eq!(identity.chinese_name.unwrap().value, "张伟");
    }

    #[test]
    fn does_not_guess_an_unlabeled_subject_name() {
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