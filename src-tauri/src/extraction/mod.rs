use std::{
    collections::{HashMap, HashSet},
    io::{Cursor, Read},
};

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
        let subject_name = message.subject.as_deref().and_then(subject_name_candidate);

        let generic_name = subject_name.clone().or_else(|| {
            capture_name_first(
                &body,
                &[
                    r"(?im)^[ \t]*(?:student\s*name|applicant\s*name|name\s*of\s*(?:student|applicant)|full\s*name|学生姓名|申请学生姓名|申请人姓名|姓名)[ \t]*(?:[:：=\-])[ \t]*([^\r\n]{2,120})[ \t]*$",
                    r"(?im)^[ \t]*(?:student\s*name|applicant\s*name|name\s*of\s*(?:student|applicant)|full\s*name|学生姓名|申请学生姓名|申请人姓名|姓名)[ \t]{2,}([^\r\n]{2,120})[ \t]*$",
                    r"(?im)^[ \t]*(?:student\s*name|applicant\s*name|name\s*of\s*(?:student|applicant)|full\s*name|学生姓名|申请学生姓名|申请人姓名|姓名)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,120})[ \t]*$",
                ],
                "labeled student name",
                0.99,
            )
        })
        .or_else(|| filename_consensus_candidate(message));

        let (mut english_name, mut chinese_name) = if subject_name.is_some() {
            (None, None)
        } else {
            (
                capture_name_first(
                    &body,
                    &[
                        r"(?im)^[ \t]*(?:english\s*(?:full\s*)?name|name\s*\(\s*english\s*\)|name\s*in\s*english|英文名|英文姓名)[ \t]*(?:[:：=\-])[ \t]*([^\r\n]{2,80})[ \t]*$",
                        r"(?im)^[ \t]*(?:english\s*(?:full\s*)?name|name\s*\(\s*english\s*\)|name\s*in\s*english|英文名|英文姓名)[ \t]{2,}([^\r\n]{2,80})[ \t]*$",
                        r"(?im)^[ \t]*(?:english\s*(?:full\s*)?name|name\s*\(\s*english\s*\)|name\s*in\s*english|英文名|英文姓名)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,80})[ \t]*$",
                    ],
                    "labeled English student name",
                    0.99,
                ),
                capture_name_first(
                    &body,
                    &[
                        r"(?im)^[ \t]*(?:chinese\s*(?:full\s*)?name|name\s*\(\s*chinese\s*\)|name\s*in\s*chinese|中文名|中文姓名|姓名\s*\(\s*中文\s*\)|姓名（中文）)[ \t]*(?:[:：=\-])[ \t]*([^\r\n]{2,40})[ \t]*$",
                        r"(?im)^[ \t]*(?:chinese\s*(?:full\s*)?name|name\s*\(\s*chinese\s*\)|name\s*in\s*chinese|中文名|中文姓名|姓名\s*\(\s*中文\s*\)|姓名（中文）)[ \t]{2,}([^\r\n]{2,40})[ \t]*$",
                        r"(?im)^[ \t]*(?:chinese\s*(?:full\s*)?name|name\s*\(\s*chinese\s*\)|name\s*in\s*chinese|中文名|中文姓名|姓名\s*\(\s*中文\s*\)|姓名（中文）)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,40})[ \t]*$",
                    ],
                    "labeled Chinese student name",
                    0.99,
                ),
            )
        };

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

        let name = chinese_name
            .clone()
            .or_else(|| english_name.clone())
            .or(generic_name);

        StudentIdentity {
            name,
            english_name,
            chinese_name,
            application_id: capture_value_first(
                &body,
                &[
                    r"(?im)^[ \t]*(?:student\s*(?:id|number|no\.?|reference)|application\s*(?:id|number|no\.?|ref(?:erence)?)|学号|学生编号|申请号|申请编号|申请ID)[ \t]*[:：#=\-]?[ \t]*([A-Z0-9][A-Z0-9._/-]{2,40})[ \t]*$",
                    r"(?im)^[ \t]*(?:student\s*(?:id|number|no\.?|reference)|application\s*(?:id|number|no\.?|ref(?:erence)?)|学号|学生编号|申请号|申请编号|申请ID)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([A-Z0-9][A-Z0-9._/-]{2,40})[ \t]*$",
                ],
                "labeled student/application identifier",
                0.99,
            ),
            date_of_birth: capture_value_first(
                &body,
                &[
                    r"(?im)^[ \t]*(?:date\s*of\s*birth|birth\s*date|dob|出生日期|生日)[ \t]*[:：=\-][ \t]*([0-9]{1,4}[./-][0-9]{1,2}[./-][0-9]{1,4})[ \t]*$",
                    r"(?im)^[ \t]*(?:date\s*of\s*birth|birth\s*date|dob|出生日期|生日)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([0-9]{1,4}[./-][0-9]{1,2}[./-][0-9]{1,4})[ \t]*$",
                ],
                "labeled date of birth",
                0.98,
            ),
            university: capture_value_first(
                &body,
                &[
                    r"(?im)^[ \t]*(?:university|institution|school|院校|学校|大学)[ \t]*[:：=\-][ \t]*([^\r\n]{2,120})[ \t]*$",
                    r"(?im)^[ \t]*(?:university|institution|school|院校|学校|大学)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,120})[ \t]*$",
                ],
                "labeled university",
                0.95,
            ),
            course: capture_value_first(
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

fn subject_name_candidate(subject: &str) -> Option<ExtractedValue> {
    let cleaned = Regex::new(r"(?i)^\s*(?:(?:re|fw|fwd)\s*:\s*)+")
        .expect("constant subject prefix regex")
        .replace(subject, "")
        .trim()
        .to_string();

    for pattern in [
        r"(?i)\bfor\s+([A-Za-z][A-Za-z .'-]{2,60})\s*$",
        r"(?i)(?:student\s*name|applicant\s*name|name)\s*[:：\-–—]\s*([A-Za-z][A-Za-z .'-]{2,60}|[\p{Han}]{2,4})\s*$",
        r"(?i)(?:学生|申请人|姓名)\s*[:：\-–—]\s*([\p{Han}]{2,4}|[A-Za-z][A-Za-z .'-]{2,60})\s*$",
    ] {
        if let Some(value) = capture_raw(&cleaned, pattern) {
            let value = clean_name_candidate(&value);
            if is_plausible_name(&value) {
                return Some(ExtractedValue {
                    value,
                    confidence: 0.995,
                    evidence: "email subject".into(),
                });
            }
        }
    }

    for delimiter in [" - ", " – ", " — ", " | ", ": ", "："] {
        if !cleaned.contains(delimiter) {
            continue;
        }
        for segment in cleaned.split(delimiter) {
            let value = clean_name_candidate(segment);
            if is_plausible_name(&value) {
                return Some(ExtractedValue {
                    value,
                    confidence: 0.99,
                    evidence: "email subject segment".into(),
                });
            }
        }
    }
    None
}

fn filename_consensus_candidate(message: &ParsedMessage) -> Option<ExtractedValue> {
    let mut counts: HashMap<String, (String, usize)> = HashMap::new();
    for attachment in &message.attachments {
        let mut seen_in_file = HashSet::new();
        for candidate in filename_name_candidates(&attachment.filename) {
            let key = normalize_candidate(&candidate);
            if key.is_empty() || !seen_in_file.insert(key.clone()) {
                continue;
            }
            let entry = counts.entry(key).or_insert((candidate, 0));
            entry.1 += 1;
        }
    }

    counts
        .into_values()
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.len().cmp(&right.0.len())))
        .map(|(value, count)| ExtractedValue {
            value,
            confidence: if count >= 2 { 0.99 } else { 0.93 },
            evidence: if count >= 2 {
                format!("attachment filename consensus across {count} files")
            } else {
                "attachment filename".into()
            },
        })
}

fn filename_name_candidates(filename: &str) -> Vec<String> {
    let stem = filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(filename);
    let labeled = Regex::new(
        r"(?i)(?:student[-_ ]*name|applicant[-_ ]*name|name|学生姓名|申请人姓名|姓名)[-_ ：:=]+([\p{Han}]{2,4}|[A-Za-z][A-Za-z .'-]{1,60})",
    )
    .expect("constant filename labeled-name regex");
    let chinese_prefix = Regex::new(
        r"^([\p{Han}]{2,4})[-_ ]+(?:护照|成绩单|申请|申请材料|简历|offer|cv|resume|passport|transcript)",
    )
    .expect("constant Chinese filename name regex");

    let mut candidates = Vec::new();
    for re in [&labeled, &chinese_prefix] {
        if let Some(captures) = re.captures(stem) {
            if let Some(value) = captures.get(1) {
                let value = clean_name_candidate(value.as_str());
                if is_plausible_name(&value) {
                    candidates.push(value);
                }
            }
        }
    }

    let normalized = stem.replace(['_', '-', '—', '–'], " ");
    let tokens = normalized
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphabetic() && ch != '\''))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    for pair in tokens.windows(2) {
        let candidate = format!("{} {}", pair[0], pair[1]);
        if is_plausible_name(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
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

    if content_type == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        || filename.ends_with(".docx")
    {
        return extract_docx_text(&attachment.bytes);
    }

    if content_type == "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        || filename.ends_with(".xlsx")
    {
        return extract_xlsx_text(&attachment.bytes);
    }

    None
}

fn extract_docx_text(bytes: &[u8]) -> Option<String> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;
    let mut document = archive.by_name("word/document.xml").ok()?;
    let mut xml = String::new();
    document.read_to_string(&mut xml).ok()?;

    let mut parts = parse_docx_table_rows(&xml);
    parts.push(strip_xml(&xml));
    let text = parts.join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn parse_docx_table_rows(xml: &str) -> Vec<String> {
    let row_re = Regex::new(r"(?is)<w:tr\b[^>]*>(.*?)</w:tr>").expect("constant Word row regex");
    let cell_re = Regex::new(r"(?is)<w:tc\b[^>]*>(.*?)</w:tc>").expect("constant Word cell regex");
    let text_re = Regex::new(r"(?is)<w:t\b[^>]*>(.*?)</w:t>").expect("constant Word text regex");
    let mut rows = Vec::new();
    for row in row_re.captures_iter(xml) {
        let Some(row_body) = row.get(1) else { continue };
        let mut cells = Vec::new();
        for cell in cell_re.captures_iter(row_body.as_str()) {
            let Some(cell_body) = cell.get(1) else { continue };
            let value = text_re
                .captures_iter(cell_body.as_str())
                .filter_map(|capture| capture.get(1))
                .map(|value| decode_xml_entities(value.as_str()))
                .collect::<String>()
                .trim()
                .to_string();
            if !value.is_empty() {
                cells.push(value);
            }
        }
        if cells.len() >= 2 {
            rows.push(format!("{}: {}", cells[0], cells[1]));
        }
        if !cells.is_empty() {
            rows.push(cells.join(" | "));
        }
    }
    rows
}

fn extract_xlsx_text(bytes: &[u8]) -> Option<String> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;
    let shared_strings = if let Ok(mut shared) = archive.by_name("xl/sharedStrings.xml") {
        let mut xml = String::new();
        if shared.read_to_string(&mut xml).is_ok() {
            parse_xlsx_shared_strings(&xml)
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let mut sheet_names = Vec::new();
    for index in 0..archive.len() {
        if let Ok(file) = archive.by_index(index) {
            let name = file.name().to_string();
            if name.starts_with("xl/worksheets/") && name.ends_with(".xml") {
                sheet_names.push(name);
            }
        }
    }

    let mut output = Vec::new();
    for name in sheet_names {
        let Ok(mut sheet) = archive.by_name(&name) else { continue };
        let mut xml = String::new();
        if sheet.read_to_string(&mut xml).is_err() {
            continue;
        }
        output.extend(parse_xlsx_rows(&xml, &shared_strings));
    }
    let text = output.join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn parse_xlsx_shared_strings(xml: &str) -> Vec<String> {
    let item_re = Regex::new(r"(?is)<si\b[^>]*>(.*?)</si>").expect("constant XLSX string regex");
    let text_re = Regex::new(r"(?is)<t\b[^>]*>(.*?)</t>").expect("constant XLSX text regex");
    item_re
        .captures_iter(xml)
        .map(|item| {
            item.get(1)
                .map(|body| {
                    text_re
                        .captures_iter(body.as_str())
                        .filter_map(|capture| capture.get(1))
                        .map(|value| decode_xml_entities(value.as_str()))
                        .collect::<String>()
                })
                .unwrap_or_default()
        })
        .collect()
}

fn parse_xlsx_rows(xml: &str, shared_strings: &[String]) -> Vec<String> {
    let row_re = Regex::new(r"(?is)<row\b[^>]*>(.*?)</row>").expect("constant XLSX row regex");
    let cell_re = Regex::new(r"(?is)<c\b([^>]*)>(.*?)</c>").expect("constant XLSX cell regex");
    let value_re = Regex::new(r"(?is)<v\b[^>]*>(.*?)</v>").expect("constant XLSX value regex");
    let inline_re = Regex::new(r"(?is)<t\b[^>]*>(.*?)</t>").expect("constant XLSX inline regex");
    let mut rows = Vec::new();
    for row in row_re.captures_iter(xml) {
        let Some(row_body) = row.get(1) else { continue };
        let mut cells = Vec::new();
        for cell in cell_re.captures_iter(row_body.as_str()) {
            let attrs = cell.get(1).map(|value| value.as_str()).unwrap_or_default();
            let body = cell.get(2).map(|value| value.as_str()).unwrap_or_default();
            let value = if attrs.contains("t=\"s\"") {
                value_re
                    .captures(body)
                    .and_then(|capture| capture.get(1))
                    .and_then(|value| value.as_str().trim().parse::<usize>().ok())
                    .and_then(|index| shared_strings.get(index).cloned())
                    .unwrap_or_default()
            } else if attrs.contains("t=\"inlineStr\"") {
                inline_re
                    .captures_iter(body)
                    .filter_map(|capture| capture.get(1))
                    .map(|value| decode_xml_entities(value.as_str()))
                    .collect::<String>()
            } else {
                value_re
                    .captures(body)
                    .and_then(|capture| capture.get(1))
                    .map(|value| decode_xml_entities(value.as_str()))
                    .unwrap_or_default()
            };
            let value = value.trim().to_string();
            if !value.is_empty() {
                cells.push(value);
            }
        }
        if cells.len() >= 2 {
            rows.push(format!("{}: {}", cells[0], cells[1]));
        }
        if !cells.is_empty() {
            rows.push(cells.join(" | "));
        }
    }
    rows
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

fn clean_value(value: &str) -> String {
    value
        .trim()
        .trim_matches(['\"', '\''])
        .to_string()
}

fn normalize_candidate(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_plausible_name(value: &str) -> bool {
    let value = value.trim();
    if value.len() < 2 || value.len() > 120 || value.chars().any(|ch| ch.is_ascii_digit()) {
        return false;
    }

    let lower = value.to_ascii_lowercase();
    let blocked_phrases = [
        "application form",
        "genuine student",
        "sg diploma",
        "curriculum vitae",
        "payment instructions",
        "student declaration",
    ];
    if blocked_phrases.iter().any(|phrase| lower.contains(phrase)) {
        return false;
    }

    let han_count = value
        .chars()
        .filter(|ch| matches!(ch, '\u{4e00}'..='\u{9fff}'))
        .count();
    if han_count > 0 {
        if value.chars().any(|ch| ch.is_ascii_alphabetic()) {
            return value.contains('/') || value.contains('／');
        }
        return (2..=8).contains(&han_count);
    }

    let blocked_tokens = [
        "application", "applications", "applicant", "student", "students", "form", "offer",
        "offers", "passport", "transcript", "resume", "cv", "diploma", "declaration", "genuine",
        "education", "agent", "nomination", "authorisation", "authorization", "bachelor", "master",
        "invoice", "university", "college", "school", "course", "program", "programme", "payment",
        "instructions", "intake", "dates", "update", "flyer", "flyers", "academic", "merit",
        "international", "scholarship", "prepaid", "officeworks", "siit", "sg", "stp", "insertpic",
        "image", "catch", "document", "documents", "material", "materials", "january", "february",
        "march", "april", "may", "june", "july", "august", "september", "october", "november",
        "december",
    ];
    let tokens = value
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphabetic() && ch != '\'' && ch != '-'))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if !(2..=5).contains(&tokens.len()) {
        return false;
    }
    if tokens.iter().any(|token| token.chars().count() < 2) {
        return false;
    }
    if tokens
        .iter()
        .any(|token| blocked_tokens.iter().any(|blocked| token.eq_ignore_ascii_case(blocked)))
    {
        return false;
    }
    tokens.iter().all(|token| token.chars().any(|ch| ch.is_alphabetic()))
}

fn capture_name_first(
    text: &str,
    patterns: &[&str],
    evidence: &str,
    confidence: f32,
) -> Option<ExtractedValue> {
    patterns
        .iter()
        .find_map(|pattern| capture_name(text, pattern, evidence, confidence))
}

fn capture_value_first(
    text: &str,
    patterns: &[&str],
    evidence: &str,
    confidence: f32,
) -> Option<ExtractedValue> {
    patterns
        .iter()
        .find_map(|pattern| capture_value(text, pattern, evidence, confidence))
}

fn capture_name(
    text: &str,
    pattern: &str,
    evidence: &str,
    confidence: f32,
) -> Option<ExtractedValue> {
    let value = clean_name_candidate(&capture_raw(text, pattern)?);
    if !is_plausible_name(&value) {
        return None;
    }
    Some(ExtractedValue {
        value,
        confidence,
        evidence: evidence.to_string(),
    })
}

fn capture_value(
    text: &str,
    pattern: &str,
    evidence: &str,
    confidence: f32,
) -> Option<ExtractedValue> {
    let value = clean_value(&capture_raw(text, pattern)?);
    if value.is_empty() {
        return None;
    }
    Some(ExtractedValue {
        value,
        confidence,
        evidence: evidence.to_string(),
    })
}

fn capture_raw(text: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(pattern).expect("constant extraction regex");
    re.captures(text)?
        .get(1)
        .map(|value| value.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(filename: &str) -> Attachment {
        Attachment {
            filename: filename.into(),
            content_type: "application/octet-stream".into(),
            bytes: Vec::new(),
        }
    }

    #[test]
    fn extracts_labeled_identity_fields() {
        let message = ParsedMessage {
            subject: Some("Application update".into()),
            text_body: concat!(
                "Student Name: Chang Rui\n",
                "Application ID: APP12345\n",
                "DOB: 2001-05-17\n"
            )
            .into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.unwrap().value, "Chang Rui");
        assert_eq!(identity.application_id.unwrap().value, "APP12345");
        assert_eq!(identity.date_of_birth.unwrap().value, "2001-05-17");
    }

    #[test]
    fn subject_name_is_highest_priority() {
        let message = ParsedMessage {
            subject: Some("UTS Offer - Shu Yan".into()),
            text_body: "Student Name: Wrong Person".into(),
            ..Default::default()
        };
        assert_eq!(DeterministicExtractor.extract(&message).name.unwrap().value, "Shu Yan");
    }

    #[test]
    fn extracts_offer_for_name_from_subject() {
        let message = ParsedMessage {
            subject: Some("Offer for Chang Rui".into()),
            ..Default::default()
        };
        assert_eq!(DeterministicExtractor.extract(&message).name.unwrap().value, "Chang Rui");
    }

    #[test]
    fn rejects_generic_business_subjects() {
        for subject in ["SG Diploma Offer", "Student Documents", "Application Update", "SIIT Application Form"] {
            let message = ParsedMessage {
                subject: Some(subject.into()),
                ..Default::default()
            };
            assert!(DeterministicExtractor.extract(&message).name.is_none(), "{subject}");
        }
    }

    #[test]
    fn filename_consensus_finds_li_baichuan() {
        let message = ParsedMessage {
            attachments: vec![
                attachment("Education agent nomination and authorisation_LI Baichuan.pdf"),
                attachment("Genuine Student Declaration_LI Baichuan.pdf"),
                attachment("澳洲大学申请信息表 June 2024_LI Baichuan.docx"),
            ],
            ..Default::default()
        };
        assert_eq!(DeterministicExtractor.extract(&message).name.unwrap().value, "LI Baichuan");
    }

    #[test]
    fn generic_document_names_are_not_people() {
        for filename in ["SG Diploma Offer.pdf", "Genuine Student Declaration.pdf", "SIIT Application Form 2026.pdf"] {
            assert!(filename_name_candidates(filename).is_empty(), "{filename}");
        }
    }

    #[test]
    fn filename_consensus_finds_wu_lanbing() {
        let message = ParsedMessage {
            attachments: vec![
                attachment("学生申请信息表-模板825_WU Lanbing.xlsx"),
                attachment("学生中文签字+拼音——真实性声明_2025_WU Lanbing.pdf"),
            ],
            ..Default::default()
        };
        assert_eq!(DeterministicExtractor.extract(&message).name.unwrap().value, "WU Lanbing");
    }

    #[test]
    fn docx_table_preserves_label_value_pair() {
        let xml = "<w:document><w:body><w:tbl><w:tr><w:tc><w:p><w:r><w:t>中文姓名</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>吴兰冰</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>";
        let rows = parse_docx_table_rows(xml);
        assert!(rows.iter().any(|row| row == "中文姓名: 吴兰冰"));
    }

    #[test]
    fn xlsx_row_preserves_label_value_pair() {
        let shared = vec!["English Name".to_string(), "WU Lanbing".to_string()];
        let xml = "<worksheet><sheetData><row><c t=\"s\"><v>0</v></c><c t=\"s\"><v>1</v></c></row></sheetData></worksheet>";
        let rows = parse_xlsx_rows(xml, &shared);
        assert!(rows.iter().any(|row| row == "English Name: WU Lanbing"));
    }

    #[test]
    fn extracts_bilingual_names_from_text_attachment() {
        let message = ParsedMessage {
            attachments: vec![Attachment {
                filename: "application.txt".into(),
                content_type: "text/plain".into(),
                bytes: "中文姓名: 常瑞\nEnglish Name: Chang Rui\n申请号: APP9988".as_bytes().to_vec(),
            }],
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.as_ref().unwrap().value, "常瑞");
        assert_eq!(identity.application_id.unwrap().value, "APP9988");
    }

    #[test]
    fn keeps_numeric_date_and_identifier_fields() {
        let message = ParsedMessage {
            text_body: "申请号: 99881234\n出生日期: 2001-05-17".into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.application_id.unwrap().value, "99881234");
        assert_eq!(identity.date_of_birth.unwrap().value, "2001-05-17");
    }
}