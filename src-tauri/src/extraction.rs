use std::{
    collections::HashMap,
    io::{Cursor, Read},
    panic::{catch_unwind, AssertUnwindSafe},
};

use pinyin::ToPinyin;
use regex::Regex;

use crate::models::{Attachment, ExtractedValue, ParsedMessage, StudentIdentity};

pub trait IdentityExtractor {
    fn extract(&self, message: &ParsedMessage) -> StudentIdentity;
}

#[derive(Default)]
pub struct DeterministicExtractor;

#[derive(Clone, Debug)]
struct NameCandidate {
    value: String,
    score: i32,
    evidence: Vec<String>,
    source_rank: u8,
}

#[derive(Default)]
struct ResolvedNames {
    preferred: Option<NameCandidate>,
    english: Option<NameCandidate>,
    chinese: Option<NameCandidate>,
}

impl IdentityExtractor for DeterministicExtractor {
    fn extract(&self, message: &ParsedMessage) -> StudentIdentity {
        let mut candidates = Vec::new();

        collect_subject_candidates(message, &mut candidates);
        collect_structured_attachment_candidates(message, &mut candidates);
        collect_filename_candidates(message, &mut candidates);
        collect_body_candidates(message, &mut candidates);

        let resolved = resolve_name_candidates(candidates);
        let name = resolved.preferred.as_ref().map(candidate_to_extracted);
        let english_name = resolved.english.as_ref().map(candidate_to_extracted);
        let chinese_name = resolved.chinese.as_ref().map(candidate_to_extracted);

        let searchable = searchable_non_name_text(message);
        StudentIdentity {
            name,
            english_name,
            chinese_name,
            application_id: capture_value_first(
                &searchable,
                &[
                    r"(?im)^[ \t]*(?:student\s*(?:id|number|no\.?|reference)|application\s*(?:id|number|no\.?|ref(?:erence)?)|学号|学生编号|申请号|申请编号|申请ID)[ \t]*[:：#=\-]?[ \t]*([A-Z0-9][A-Z0-9._/-]{2,40})[ \t]*$",
                    r"(?im)^[ \t]*(?:student\s*(?:id|number|no\.?|reference)|application\s*(?:id|number|no\.?|ref(?:erence)?)|学号|学生编号|申请号|申请编号|申请ID)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([A-Z0-9][A-Z0-9._/-]{2,40})[ \t]*$",
                ],
                "labeled student/application identifier",
                0.99,
            ),
            date_of_birth: capture_value_first(
                &searchable,
                &[
                    r"(?im)^[ \t]*(?:date\s*of\s*birth|birth\s*date|dob|出生日期|生日)[ \t]*[:：=\-][ \t]*([0-9]{1,4}[./-][0-9]{1,2}[./-][0-9]{1,4})[ \t]*$",
                    r"(?im)^[ \t]*(?:date\s*of\s*birth|birth\s*date|dob|出生日期|生日)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([0-9]{1,4}[./-][0-9]{1,2}[./-][0-9]{1,4})[ \t]*$",
                ],
                "labeled date of birth",
                0.98,
            ),
            university: capture_value_first(
                &searchable,
                &[
                    r"(?im)^[ \t]*(?:university|institution|school|院校|学校|大学)[ \t]*[:：=\-][ \t]*([^\r\n]{2,120})[ \t]*$",
                    r"(?im)^[ \t]*(?:university|institution|school|院校|学校|大学)[ \t]*(?:[:：])?[ \t]*\r?\n[ \t]*([^\r\n]{2,120})[ \t]*$",
                ],
                "labeled university",
                0.95,
            ),
            course: capture_value_first(
                &searchable,
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

fn candidate_to_extracted(candidate: &NameCandidate) -> ExtractedValue {
    ExtractedValue {
        value: candidate.value.clone(),
        confidence: score_to_confidence(candidate.score),
        evidence: candidate.evidence.join("; "),
    }
}

fn collect_subject_candidates(message: &ParsedMessage, candidates: &mut Vec<NameCandidate>) {
    let Some(subject) = message.subject.as_deref() else {
        return;
    };
    let cleaned = Regex::new(r"(?i)^\s*(?:(?:re|fw|fwd)\s*:\s*)+")
        .expect("constant subject prefix regex")
        .replace(subject, "")
        .trim()
        .to_string();

    for pattern in [
        r"(?i)(?:student\s*name|applicant\s*name|name|学生姓名|申请人姓名|姓名)\s*[:：\-–—]\s*([\p{Han}]{2,8}|[A-Za-z][A-Za-z .'-]{2,60})",
        r"(?i)\bfor\s+([A-Za-z][A-Za-z .'-]{2,60})\s*$",
        r"([\p{Han}]{2,4})\s*(?:的)?(?:申请|材料|offer|录取|签证|护照|成绩单)",
    ] {
        if let Some(value) = capture_raw(&cleaned, pattern) {
            push_candidate(candidates, &value, 108, 0, "explicit email subject");
        }
    }

    for delimiter in [" - ", " – ", " — ", " | ", ": ", "："] {
        if !cleaned.contains(delimiter) {
            continue;
        }
        for segment in cleaned.split(delimiter) {
            push_candidate(candidates, segment, 98, 0, "email subject segment");
        }
    }

    collect_entity_candidates_from_text(
        &cleaned,
        102,
        0,
        "person entity recognized in email subject",
        candidates,
    );
}

fn collect_structured_attachment_candidates(
    message: &ParsedMessage,
    candidates: &mut Vec<NameCandidate>,
) {
    for attachment in &message.attachments {
        let filename = attachment.filename.to_ascii_lowercase();
        let content_type = attachment.content_type.to_ascii_lowercase();
        let extracted = if filename.ends_with(".docx")
            || content_type
                == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        {
            extract_docx_text(&attachment.bytes)
        } else if filename.ends_with(".xlsx")
            || content_type
                == "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        {
            extract_xlsx_text(&attachment.bytes)
        } else if filename.ends_with(".pdf") || content_type == "application/pdf" {
            safe_pdf_text(&attachment.bytes)
        } else {
            None
        };

        let Some(text) = extracted else {
            continue;
        };
        for (label, value) in labeled_name_pairs(&text) {
            let label_lower = label.to_ascii_lowercase();
            let score = if label_lower.contains("chinese")
                || label.contains("中文")
                || label.contains("姓名")
            {
                104
            } else {
                101
            };
            push_candidate(
                candidates,
                &value,
                score,
                1,
                &format!(
                    "{} structured field in {}",
                    label.trim(),
                    attachment.filename
                ),
            );
        }

        collect_entity_candidates_from_text(
            &text,
            68,
            4,
            &format!("person entity recognized in {}", attachment.filename),
            candidates,
        );
    }
}

fn collect_filename_candidates(message: &ParsedMessage, candidates: &mut Vec<NameCandidate>) {
    let mut counts: HashMap<String, (String, usize)> = HashMap::new();
    for attachment in &message.attachments {
        for value in filename_name_candidates(&attachment.filename) {
            let key = normalize_candidate(&value);
            if key.is_empty() {
                continue;
            }
            let entry = counts.entry(key).or_insert((value, 0));
            entry.1 += 1;
        }
    }

    for (_, (value, count)) in counts {
        let score = if count >= 3 {
            101
        } else if count == 2 {
            97
        } else {
            82
        };
        push_candidate(
            candidates,
            &value,
            score,
            2,
            &format!("attachment filename consensus across {count} file(s)"),
        );
    }
}

fn collect_body_candidates(message: &ParsedMessage, candidates: &mut Vec<NameCandidate>) {
    let mut body = message.text_body.clone();
    if !message.html_body.is_empty() {
        body.push('\n');
        body.push_str(&strip_html(&message.html_body));
    }

    for (label, value) in labeled_name_pairs(&body) {
        push_candidate(
            candidates,
            &value,
            94,
            3,
            &format!("{} in email body", label.trim()),
        );
    }

    collect_entity_candidates_from_text(
        &body,
        70,
        5,
        "person entity recognized in email body",
        candidates,
    );
}

fn collect_entity_candidates_from_text(
    text: &str,
    score: i32,
    source_rank: u8,
    evidence: &str,
    candidates: &mut Vec<NameCandidate>,
) {
    let latin_re = Regex::new(
        r"\b(?:[A-Z]{2,20}|[A-Z][a-z]{1,30})(?:[ _-]+(?:[A-Z]{2,20}|[A-Z][a-z]{1,30})){1,2}\b",
    )
    .expect("constant Latin person entity regex");
    for entity in latin_re.find_iter(text) {
        push_candidate(candidates, entity.as_str(), score, source_rank, evidence);
    }

    let chinese_context_patterns = [
        r"(?:学生|申请人|同学|关于|有关)\s*([\p{Han}]{2,4})(?:的|\s|$)",
        r"([\p{Han}]{2,4})\s*的(?:申请|材料|录取|签证|护照|成绩单|文件)",
        r"(?:申请|材料|录取|签证|护照|成绩单|文件)\s*[-—–:：]?\s*([\p{Han}]{2,4})(?:\s|$)",
    ];
    for pattern in chinese_context_patterns {
        let re = Regex::new(pattern).expect("constant Chinese person entity regex");
        for captures in re.captures_iter(text) {
            if let Some(entity) = captures.get(1) {
                push_candidate(candidates, entity.as_str(), score, source_rank, evidence);
            }
        }
    }
}

fn labeled_name_pairs(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let labels = [
        "student name",
        "applicant name",
        "name of student",
        "name of applicant",
        "full name",
        "english name",
        "name in english",
        "chinese name",
        "name in chinese",
        "学生姓名",
        "申请学生姓名",
        "申请人姓名",
        "中文姓名",
        "英文姓名",
        "中文名",
        "英文名",
        "姓名（中文）",
        "姓名(中文)",
        "姓名",
    ];
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        for label in labels {
            let label_lower = label.to_ascii_lowercase();
            if lower == label_lower
                || lower.starts_with(&(label_lower.clone() + ":"))
                || lower.starts_with(&(label_lower.clone() + "："))
            {
                if let Some((_, value)) = line.split_once(':').or_else(|| line.split_once('：')) {
                    if is_plausible_name(value) {
                        pairs.push((label.to_string(), clean_name_candidate(value)));
                    }
                } else if let Some(next) = lines.get(index + 1) {
                    if is_plausible_name(next) {
                        pairs.push((label.to_string(), clean_name_candidate(next)));
                    }
                }
            }
        }
    }
    pairs
}

fn filename_name_candidates(filename: &str) -> Vec<String> {
    let stem = filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(filename);
    let mut candidates = Vec::new();

    for pattern in [
        r"(?i)(?:student[-_ ]*name|applicant[-_ ]*name|name|学生姓名|申请人姓名|姓名)[-_ ：:=]+([\p{Han}]{2,8}|[A-Za-z][A-Za-z .'-]{2,60})",
        r"^([\p{Han}]{2,8})[-_ ]+(?:护照|成绩单|申请|申请材料|简历|offer|cv|resume|passport|transcript)",
        r"(?i)[_\-–—]([A-Z][A-Za-z'-]{1,30}[ _-]+[A-Z][A-Za-z'-]{1,30})$",
        r"(?i)[_\-–—]([A-Z]{2,20}[ _-]+[A-Z][A-Za-z'-]{1,30})$",
        r"(?i)(?:offer|application|form)[-_ ]+([A-Za-z][A-Za-z'-]{1,30})[-_ ]+([A-Za-z][A-Za-z'-]{1,30})(?:[-_ ]+[A-Z0-9]{4,})?",
    ] {
        let re = Regex::new(pattern).expect("constant filename name regex");
        if let Some(captures) = re.captures(stem) {
            let value = if captures.len() >= 3 {
                format!(
                    "{} {}",
                    captures.get(1).map(|m| m.as_str()).unwrap_or_default(),
                    captures.get(2).map(|m| m.as_str()).unwrap_or_default()
                )
            } else {
                captures
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            if is_plausible_name(&value) {
                candidates.push(clean_name_candidate(&value.replace(['_', '-'], " ")));
            }
        }
    }

    let normalized = stem.replace(['_', '-', '—', '–'], " ");
    let tokens = normalized
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphabetic() && ch != '\''))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    for width in [2usize, 3] {
        for window in tokens.windows(width) {
            let value = window.join(" ");
            if is_plausible_name(&value) {
                candidates.push(value);
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn resolve_name_candidates(candidates: Vec<NameCandidate>) -> ResolvedNames {
    let mut merged = merge_exact_candidates(candidates);
    if merged.is_empty() {
        return ResolvedNames::default();
    }

    let keys = merged.keys().cloned().collect::<Vec<_>>();
    let mut pinyin_pairs = Vec::new();
    for chinese_key in &keys {
        let Some(chinese) = merged.get(chinese_key) else {
            continue;
        };
        if !contains_han(&chinese.value) {
            continue;
        }
        for english_key in &keys {
            let Some(english) = merged.get(english_key) else {
                continue;
            };
            if contains_han(&english.value) {
                continue;
            }
            if pinyin_equivalent(&chinese.value, &english.value) {
                pinyin_pairs.push((
                    chinese_key.clone(),
                    english_key.clone(),
                    chinese.score + english.score,
                ));
            }
        }
    }

    pinyin_pairs.sort_by_key(|left| std::cmp::Reverse(left.2));
    if let Some((chinese_key, english_key, pair_score)) = pinyin_pairs.first().cloned() {
        let runner_up = pinyin_pairs.get(1).map(|pair| pair.2).unwrap_or(i32::MIN);
        if pair_score >= 145 && pair_score.saturating_sub(runner_up) >= 6 {
            let mut chinese = merged.remove(&chinese_key).expect("candidate key exists");
            let mut english = merged.remove(&english_key).expect("candidate key exists");
            chinese.score = (chinese.score + english.score / 3 + 20).min(140);
            english.score = (english.score + chinese.score / 6 + 8).min(130);
            chinese.evidence.push(format!(
                "pinyin corroboration with English name {}",
                english.value
            ));
            english.evidence.push(format!(
                "pinyin corroboration with Chinese name {}",
                chinese.value
            ));
            return ResolvedNames {
                preferred: Some(chinese.clone()),
                english: Some(english),
                chinese: Some(chinese),
            };
        }
    }

    let mut values = merged.into_values().collect::<Vec<_>>();
    values.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.source_rank.cmp(&right.source_rank))
            .then_with(|| contains_han(&right.value).cmp(&contains_han(&left.value)))
    });
    let Some(best) = values.first().cloned() else {
        return ResolvedNames::default();
    };
    if best.score < 88 {
        return ResolvedNames::default();
    }
    if let Some(second) = values.get(1) {
        if best.score.saturating_sub(second.score) < 6 {
            return ResolvedNames::default();
        }
    }

    if contains_han(&best.value) {
        ResolvedNames {
            preferred: Some(best.clone()),
            english: None,
            chinese: Some(best),
        }
    } else {
        ResolvedNames {
            preferred: Some(best.clone()),
            english: Some(best),
            chinese: None,
        }
    }
}

fn merge_exact_candidates(candidates: Vec<NameCandidate>) -> HashMap<String, NameCandidate> {
    let mut merged: HashMap<String, NameCandidate> = HashMap::new();
    for candidate in candidates {
        let key = normalize_candidate(&candidate.value);
        if key.is_empty() {
            continue;
        }
        merged
            .entry(key)
            .and_modify(|existing| {
                existing.score = (existing.score + candidate.score / 2).min(130);
                existing.source_rank = existing.source_rank.min(candidate.source_rank);
                existing.evidence.extend(candidate.evidence.clone());
            })
            .or_insert(candidate);
    }
    merged
}

fn pinyin_equivalent(chinese: &str, latin: &str) -> bool {
    let latin_key = normalize_candidate(latin);
    if latin_key.is_empty() {
        return false;
    }
    chinese_pinyin_forms(chinese)
        .iter()
        .any(|form| form == &latin_key)
}

fn chinese_pinyin_forms(chinese: &str) -> Vec<String> {
    let mut syllables = Vec::new();
    for ch in chinese.chars().filter(|ch| matches!(ch, '\u{4e00}'..='\u{9fff}')) {
        let Some(pinyin) = ch.to_pinyin() else {
            return Vec::new();
        };
        syllables.push(pinyin.plain().to_ascii_lowercase());
    }
    if syllables.len() < 2 {
        return Vec::new();
    }

    let direct = syllables.concat();
    let mut forms = vec![direct];
    if syllables.len() >= 2 {
        let given_first = format!("{}{}", syllables[1..].concat(), syllables[0]);
        if !forms.contains(&given_first) {
            forms.push(given_first);
        }
    }
    forms
}

fn push_candidate(
    candidates: &mut Vec<NameCandidate>,
    value: &str,
    score: i32,
    source_rank: u8,
    evidence: &str,
) {
    let value = clean_name_candidate(value);
    if !is_plausible_name(&value) {
        return;
    }
    candidates.push(NameCandidate {
        value,
        score,
        evidence: vec![evidence.to_string()],
        source_rank,
    });
}

fn searchable_non_name_text(message: &ParsedMessage) -> String {
    let mut parts = Vec::new();
    if !message.text_body.is_empty() {
        parts.push(message.text_body.clone());
    }
    if !message.html_body.is_empty() {
        parts.push(strip_html(&message.html_body));
    }
    for attachment in &message.attachments {
        if let Some(text) = extract_attachment_text(attachment) {
            parts.push(text);
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
        return Some(String::from_utf8_lossy(bytes).into_owned());
    }
    if filename.ends_with(".pdf") || content_type == "application/pdf" {
        return safe_pdf_text(&attachment.bytes);
    }
    if filename.ends_with(".docx")
        || content_type
            == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    {
        return extract_docx_text(&attachment.bytes);
    }
    if filename.ends_with(".xlsx")
        || content_type == "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    {
        return extract_xlsx_text(&attachment.bytes);
    }
    None
}

fn safe_pdf_text(bytes: &[u8]) -> Option<String> {
    catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text_from_mem(bytes)))
        .ok()
        .and_then(Result::ok)
        .filter(|text| !text.trim().is_empty())
}

fn extract_docx_text(bytes: &[u8]) -> Option<String> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;
    let mut document = archive.by_name("word/document.xml").ok()?;
    let mut xml = String::new();
    document.read_to_string(&mut xml).ok()?;

    let row_re = Regex::new(r"(?is)<w:tr\b[^>]*>(.*?)</w:tr>").expect("constant Word row regex");
    let cell_re = Regex::new(r"(?is)<w:tc\b[^>]*>(.*?)</w:tc>").expect("constant Word cell regex");
    let text_re = Regex::new(r"(?is)<w:t\b[^>]*>(.*?)</w:t>").expect("constant Word text regex");
    let mut output = Vec::new();
    for row in row_re.captures_iter(&xml) {
        let Some(row_body) = row.get(1) else {
            continue;
        };
        let cells = cell_re
            .captures_iter(row_body.as_str())
            .filter_map(|cell| cell.get(1))
            .map(|cell| {
                text_re
                    .captures_iter(cell.as_str())
                    .filter_map(|capture| capture.get(1))
                    .map(|value| decode_xml_entities(value.as_str()))
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if cells.len() >= 2 {
            output.push(format!("{}: {}", cells[0], cells[1]));
        }
        if !cells.is_empty() {
            output.push(cells.join(" | "));
        }
    }
    output.push(strip_xml(&xml));
    let text = output.join("\n");
    (!text.trim().is_empty()).then_some(text)
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
        let Ok(mut sheet) = archive.by_name(&name) else {
            continue;
        };
        let mut xml = String::new();
        if sheet.read_to_string(&mut xml).is_ok() {
            output.extend(parse_xlsx_rows(&xml, &shared_strings));
        }
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
        let Some(row_body) = row.get(1) else {
            continue;
        };
        let mut cells = Vec::new();
        for cell in cell_re.captures_iter(row_body.as_str()) {
            let attrs = cell.get(1).map(|m| m.as_str()).unwrap_or_default();
            let body = cell.get(2).map(|m| m.as_str()).unwrap_or_default();
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

fn clean_name_candidate(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| ch.is_whitespace() || "/|,;()（）[]{}:：".contains(ch))
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_han(value: &str) -> bool {
    value
        .chars()
        .any(|ch| matches!(ch, '\u{4e00}'..='\u{9fff}'))
}

fn normalize_candidate(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_plausible_name(value: &str) -> bool {
    let value = clean_name_candidate(value);
    if value.len() < 2 || value.len() > 80 || value.chars().any(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    let blocked_phrases = [
        "student",
        "student name",
        "applicant",
        "application",
        "application form",
        "genuine student",
        "student declaration",
        "sg diploma",
        "siit",
        "diploma",
        "passport",
        "transcript",
        "offer",
        "invoice",
        "payment instructions",
        "education agent",
        "curriculum vitae",
        "学生",
        "申请人",
        "申请材料",
        "申请信息",
        "护照",
        "成绩单",
        "声明",
        "真实性声明",
        "中文签字",
        "拼音",
        "姓名",
        "中文姓名",
        "英文姓名",
        "大学",
        "学校",
        "课程",
        "专业",
    ];
    if blocked_phrases
        .iter()
        .any(|phrase| lower == *phrase || lower.contains(phrase))
    {
        return false;
    }
    if contains_han(&value) {
        let han_count = value
            .chars()
            .filter(|ch| matches!(ch, '\u{4e00}'..='\u{9fff}'))
            .count();
        return (2..=4).contains(&han_count);
    }
    let blocked_tokens = [
        "application",
        "applicant",
        "student",
        "form",
        "offer",
        "passport",
        "transcript",
        "resume",
        "cv",
        "diploma",
        "declaration",
        "genuine",
        "education",
        "agent",
        "nomination",
        "authorisation",
        "authorization",
        "bachelor",
        "master",
        "invoice",
        "university",
        "college",
        "school",
        "course",
        "program",
        "programme",
        "payment",
        "instructions",
        "intake",
        "dates",
        "update",
        "flyer",
        "academic",
        "merit",
        "international",
        "scholarship",
        "officeworks",
        "siit",
        "sg",
        "stp",
        "insertpic",
        "image",
        "catch",
        "document",
        "documents",
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let tokens = value
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| !ch.is_alphabetic() && ch != '\'' && ch != '-')
        })
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if !(2..=4).contains(&tokens.len()) {
        return false;
    }
    if tokens.iter().any(|token| token.chars().count() < 2) {
        return false;
    }
    !tokens.iter().any(|token| {
        blocked_tokens
            .iter()
            .any(|blocked| token.eq_ignore_ascii_case(blocked))
    })
}

fn score_to_confidence(score: i32) -> f32 {
    match score {
        108.. => 0.999,
        100..=107 => 0.995,
        96..=99 => 0.99,
        92..=95 => 0.97,
        88..=91 => 0.93,
        _ => 0.0,
    }
}

fn capture_value_first(
    text: &str,
    patterns: &[&str],
    evidence: &str,
    confidence: f32,
) -> Option<ExtractedValue> {
    patterns
        .iter()
        .find_map(|pattern| capture_raw(text, pattern))
        .map(|value| ExtractedValue {
            value: value.trim().trim_matches(['\"', '\'']).to_string(),
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
    fn explicit_subject_name_wins() {
        let message = ParsedMessage {
            subject: Some("UTS Offer - Shu Yan".into()),
            attachments: vec![attachment("SG Diploma Offer.pdf")],
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.unwrap().value, "Shu Yan");
    }

    #[test]
    fn subject_entity_without_name_label_is_recognized() {
        let message = ParsedMessage {
            subject: Some("Application update - LI Baichuan".into()),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.unwrap().value, "LI Baichuan");
    }

    #[test]
    fn filename_consensus_extracts_li_baichuan() {
        let message = ParsedMessage {
            attachments: vec![
                attachment("Education agent nomination and authorisation_LI Baichuan.pdf"),
                attachment("Genuine Student Declaration_LI Baichuan.pdf"),
                attachment("澳洲大学申请信息表 June 2024_LI Baichuan.docx"),
            ],
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.unwrap().value, "LI Baichuan");
    }

    #[test]
    fn filename_suffix_extracts_wu_lanbing() {
        let message = ParsedMessage {
            attachments: vec![
                attachment("学生申请信息表-模板825_WU Lanbing.xlsx"),
                attachment("学生中文签字+拼音——真实性声明_2025_WU Lanbing.pdf"),
            ],
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.unwrap().value, "WU Lanbing");
    }

    #[test]
    fn pinyin_corroboration_prefers_chinese_canonical_name() {
        let message = ParsedMessage {
            subject: Some("Application - WU Lanbing".into()),
            text_body: "中文姓名：吴兰冰".into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.as_ref().unwrap().value, "吴兰冰");
        assert_eq!(identity.chinese_name.as_ref().unwrap().value, "吴兰冰");
        assert_eq!(identity.english_name.as_ref().unwrap().value, "WU Lanbing");
    }

    #[test]
    fn pinyin_corroboration_accepts_given_name_first_order() {
        assert!(pinyin_equivalent("李百川", "Baichuan Li"));
        assert!(pinyin_equivalent("李百川", "LI Baichuan"));
    }

    #[test]
    fn unlabeled_entity_needs_corroboration_outside_subject() {
        let message = ParsedMessage {
            text_body: "Please process LI Baichuan application materials.".into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert!(identity.name.is_none());
    }

    #[test]
    fn generic_document_terms_are_rejected() {
        for value in ["Student", "SG Diploma", "SIIT", "Genuine Student"] {
            assert!(!is_plausible_name(value));
        }
    }

    #[test]
    fn adjacent_table_lines_extract_name() {
        let message = ParsedMessage {
            text_body: "申请人姓名\n张伟\nEnglish Name\nWei Zhang".into(),
            ..Default::default()
        };
        let identity = DeterministicExtractor.extract(&message);
        assert_eq!(identity.name.unwrap().value, "张伟");
    }
}
