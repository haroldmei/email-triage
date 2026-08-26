from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"Expected patch anchor not found in {path}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


replace_once(
    "src-tauri/src/lib.rs",
    "pub mod logging;\npub mod mail;",
    "pub mod logging;\npub mod local_ml;\npub mod mail;",
)

# Let the ML bridge reuse the exact same local document text extraction as deterministic extraction.
replace_once(
    "src-tauri/src/extraction.rs",
    "fn extract_attachment_text(attachment: &Attachment) -> Option<String> {",
    "pub(crate) fn extract_attachment_text(attachment: &Attachment) -> Option<String> {",
)

# Use local OCR/NER enrichment and merge an accepted ML name structurally into StudentIdentity.
replace_once(
    "src-tauri/src/workflow/mod.rs",
    "    let raw_digest = Sha256::digest(&fetched.raw);\n    let identity = DeterministicExtractor.extract(&message);\n",
    '''    let raw_digest = Sha256::digest(&fetched.raw);
    let enrichment = crate::local_ml::enrich_message(app, fetched.uid, &message).await;
    let mut identity = DeterministicExtractor.extract(&enrichment.message);
    if let Some(ml_name) = enrichment.student_name.as_deref() {
        let extracted = ExtractedValue {
            value: ml_name.to_string(),
            confidence: enrichment.confidence,
            evidence: format!("local ML: {}", enrichment.evidence),
        };
        identity.name = Some(extracted.clone());
        if ml_name.chars().any(|ch| matches!(ch, '\\u{4e00}'..='\\u{9fff}')) {
            identity.chinese_name = Some(extracted);
        } else {
            identity.english_name = Some(extracted);
        }
    }
''',
)

# Local-ML candidates have already passed the ML acceptance threshold; don't discard 0.85-0.90
# candidates just because the deterministic path normally requires 0.90.
old_preferred = '''fn preferred_student_name(identity: &StudentIdentity) -> Option<&ExtractedValue> {
    identity
        .chinese_name
        .as_ref()
        .filter(|value| value.confidence >= 0.9)
        .or_else(|| {
            identity
                .english_name
                .as_ref()
                .filter(|value| value.confidence >= 0.9)
        })
        .or_else(|| identity.name.as_ref().filter(|value| value.confidence >= 0.9))
}
'''
new_preferred = '''fn preferred_student_name(identity: &StudentIdentity) -> Option<&ExtractedValue> {
    let accepted = |value: &&ExtractedValue| {
        value.confidence >= 0.9
            || (value.confidence >= 0.85 && value.evidence.starts_with("local ML:"))
    };
    identity
        .chinese_name
        .as_ref()
        .filter(accepted)
        .or_else(|| identity.english_name.as_ref().filter(accepted))
        .or_else(|| identity.name.as_ref().filter(accepted))
}
'''
replace_once("src-tauri/src/workflow/mod.rs", old_preferred, new_preferred)

# Diagnostics: XLSX support and panic-safe PDF extraction.
replace_once(
    "src-tauri/src/workflow/mod.rs",
    '''    if content_type == "application/pdf" || filename.ends_with(".pdf") {
        return match pdf_extract::extract_text_from_mem(&attachment.bytes) {
            Ok(text) if !text.trim().is_empty() => AttachmentTextDiagnostic {''',
    '''    if content_type == "application/pdf" || filename.ends_with(".pdf") {
        return match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pdf_extract::extract_text_from_mem(&attachment.bytes))).ok().and_then(Result::ok) {
            Some(text) if !text.trim().is_empty() => AttachmentTextDiagnostic {''',
)
replace_once(
    "src-tauri/src/workflow/mod.rs",
    "    if content_type\n        == \"application/vnd.openxmlformats-officedocument.wordprocessingml.document\"\n        || filename.ends_with(\".docx\")\n    {\n        return match diagnostic_docx_text_chars(&attachment.bytes) {",
    "    if content_type == \"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\"\n        || filename.ends_with(\".xlsx\")\n    {\n        return match diagnostic_xlsx_text_chars(&attachment.bytes) {\n            Some(text_chars) if text_chars > 0 => AttachmentTextDiagnostic {\n                kind: \"xlsx\",\n                extractable: true,\n                extracted: true,\n                text_chars,\n                reason: \"ok\",\n            },\n            _ => AttachmentTextDiagnostic {\n                kind: \"xlsx\",\n                extractable: true,\n                extracted: false,\n                text_chars: 0,\n                reason: \"workbook_unavailable_or_empty\",\n            },\n        };\n    }\n\n    if content_type\n        == \"application/vnd.openxmlformats-officedocument.wordprocessingml.document\"\n        || filename.ends_with(\".docx\")\n    {\n        return match diagnostic_docx_text_chars(&attachment.bytes) {",
)
replace_once(
    "src-tauri/src/workflow/mod.rs",
    "fn diagnostic_docx_text_chars(bytes: &[u8]) -> Option<usize> {",
    "fn diagnostic_xlsx_text_chars(bytes: &[u8]) -> Option<usize> {\n    let cursor = Cursor::new(bytes);\n    let mut archive = zip::ZipArchive::new(cursor).ok()?;\n    let mut total = 0usize;\n    for index in 0..archive.len() {\n        let mut file = archive.by_index(index).ok()?;\n        let name = file.name().to_string();\n        if !(name == \"xl/sharedStrings.xml\" || (name.starts_with(\"xl/worksheets/sheet\") && name.ends_with(\".xml\"))) { continue; }\n        let mut xml = String::new();\n        if file.read_to_string(&mut xml).is_ok() {\n            let tag_re = Regex::new(r\"(?is)<[^>]+>\").expect(\"constant XML diagnostic regex\");\n            total += tag_re.replace_all(&xml, \"\").trim().chars().count();\n        }\n    }\n    Some(total)\n}\n\nfn diagnostic_docx_text_chars(bytes: &[u8]) -> Option<usize> {",
)

# All extractable text types feed deterministic candidate extraction too.
old_structured = '''        let filename = attachment.filename.to_ascii_lowercase();
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
'''
replace_once("src-tauri/src/extraction.rs", old_structured, "        let extracted = extract_attachment_text(attachment);\n")

# Block observed non-name phrases and OCR error tokens.
replace_once(
    "src-tauri/src/extraction.rs",
    "    let value = clean_name_candidate(value);\n    if !is_plausible_name(&value) {\n",
    "    let value = clean_name_candidate(value);\n    if is_blocked_name(&value) || !is_plausible_name(&value) {\n",
)
replace_once(
    "src-tauri/src/extraction.rs",
    "fn searchable_non_name_text(message: &ParsedMessage) -> String {",
    '''fn is_blocked_name(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "选择对应" | "点击查看" | "查看详情" | "申请材料" | "相关文件"
            | "学生信息" | "申请状态" | "上传文件" | "查看文件" | "附件信息"
            | "申请信息" | "文件材料" | "对应材料" | "ocr error" | "order no"
            | "order number" | "shinyway sydney"
    )
}

fn searchable_non_name_text(message: &ParsedMessage) -> String {''',
)

# Repetition from the same weak source must not manufacture confidence. Only cross-source
# corroboration may raise a candidate score.
replace_once(
    "src-tauri/src/extraction.rs",
    '''            .and_modify(|existing| {
                existing.score = (existing.score + candidate.score / 2).min(130);
                existing.source_rank = existing.source_rank.min(candidate.source_rank);
                existing.evidence.extend(candidate.evidence.clone());
            })''',
    '''            .and_modify(|existing| {
                if existing.source_rank != candidate.source_rank {
                    existing.score = (existing.score + candidate.score / 3).min(130);
                    existing.source_rank = existing.source_rank.min(candidate.source_rank);
                }
                if !existing.evidence.iter().any(|item| candidate.evidence.contains(item)) {
                    existing.evidence.extend(candidate.evidence.clone());
                }
            })''',
)

replace_once(
    "src-tauri/Cargo.toml",
    'chrono = { version = "0.4", default-features = false, features = ["clock"] }\n',
    'chrono = { version = "0.4", default-features = false, features = ["clock"] }\nbase64 = "0.22"\n',
)

print("Applied local OCR/NER integration patch v0.1.20")
