from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"Expected patch anchor not found in {path}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


# Wire the local ML module into the Rust crate.
replace_once(
    "src-tauri/src/lib.rs",
    "pub mod logging;\npub mod mail;",
    "pub mod logging;\npub mod local_ml;\npub mod mail;",
)

# Use local OCR/NER enrichment before deterministic resolution. The original message is retained
# for attachment upload, while enriched text is used only for identity extraction.
replace_once(
    "src-tauri/src/workflow/mod.rs",
    "    let raw_digest = Sha256::digest(&fetched.raw);\n    let identity = DeterministicExtractor.extract(&message);\n",
    "    let raw_digest = Sha256::digest(&fetched.raw);\n    let enriched_message = crate::local_ml::enrich_message(app, fetched.uid, &message).await;\n    let identity = DeterministicExtractor.extract(&enriched_message);\n",
)

# Keep attachment diagnostics consistent with actual XLSX extraction support.
replace_once(
    "src-tauri/src/workflow/mod.rs",
    "    if content_type\n        == \"application/vnd.openxmlformats-officedocument.wordprocessingml.document\"\n        || filename.ends_with(\".docx\")\n    {\n        return match diagnostic_docx_text_chars(&attachment.bytes) {",
    "    if content_type == \"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\"\n        || filename.ends_with(\".xlsx\")\n    {\n        return match diagnostic_xlsx_text_chars(&attachment.bytes) {\n            Some(text_chars) if text_chars > 0 => AttachmentTextDiagnostic {\n                kind: \"xlsx\",\n                extractable: true,\n                extracted: true,\n                text_chars,\n                reason: \"ok\",\n            },\n            _ => AttachmentTextDiagnostic {\n                kind: \"xlsx\",\n                extractable: true,\n                extracted: false,\n                text_chars: 0,\n                reason: \"workbook_unavailable_or_empty\",\n            },\n        };\n    }\n\n    if content_type\n        == \"application/vnd.openxmlformats-officedocument.wordprocessingml.document\"\n        || filename.ends_with(\".docx\")\n    {\n        return match diagnostic_docx_text_chars(&attachment.bytes) {",
)
replace_once(
    "src-tauri/src/workflow/mod.rs",
    "fn diagnostic_docx_text_chars(bytes: &[u8]) -> Option<usize> {",
    "fn diagnostic_xlsx_text_chars(bytes: &[u8]) -> Option<usize> {\n    let cursor = Cursor::new(bytes);\n    let mut archive = zip::ZipArchive::new(cursor).ok()?;\n    let mut total = 0usize;\n    for index in 0..archive.len() {\n        let mut file = archive.by_index(index).ok()?;\n        let name = file.name().to_string();\n        if !(name == \"xl/sharedStrings.xml\" || (name.starts_with(\"xl/worksheets/sheet\") && name.ends_with(\".xml\"))) {\n            continue;\n        }\n        let mut xml = String::new();\n        if file.read_to_string(&mut xml).is_ok() {\n            let tag_re = Regex::new(r\"(?is)<[^>]+>\").expect(\"constant XML diagnostic regex\");\n            total += tag_re.replace_all(&xml, \"\").trim().chars().count();\n        }\n    }\n    Some(total)\n}\n\nfn diagnostic_docx_text_chars(bytes: &[u8]) -> Option<usize> {",
)

# All locally extractable text attachments (CSV/TXT/JSON/XML/HTML/PDF/DOCX/XLSX) now feed the
# same identity candidate pipeline, instead of CSV being extracted for diagnostics but ignored.
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
replace_once(
    "src-tauri/src/extraction.rs",
    old_structured,
    "        let extracted = extract_attachment_text(attachment);\n",
)

# Reject known UI/action phrases before they can accumulate repeated-body evidence and become a
# false high-confidence Chinese name (the observed \"选择对应\" defect).
replace_once(
    "src-tauri/src/extraction.rs",
    "    let value = clean_name_candidate(value);\n    if !is_plausible_name(&value) {\n",
    "    let value = clean_name_candidate(value);\n    if is_blocked_name(&value) || !is_plausible_name(&value) {\n",
)
replace_once(
    "src-tauri/src/extraction.rs",
    "fn searchable_non_name_text(message: &ParsedMessage) -> String {",
    '''fn is_blocked_name(value: &str) -> bool {
    matches!(
        value.trim(),
        "选择对应"
            | "点击查看"
            | "查看详情"
            | "申请材料"
            | "相关文件"
            | "学生信息"
            | "申请状态"
            | "上传文件"
            | "查看文件"
            | "附件信息"
            | "申请信息"
            | "文件材料"
            | "对应材料"
    )
}

fn searchable_non_name_text(message: &ParsedMessage) -> String {''',
)

# Local ML payloads use base64 to avoid temporary files and keep the worker localhost-only.
replace_once(
    "src-tauri/Cargo.toml",
    'chrono = { version = "0.4", default-features = false, features = ["clock"] }\n',
    'chrono = { version = "0.4", default-features = false, features = ["clock"] }\nbase64 = "0.22"\n',
)

print("Applied local OCR/NER integration patch")
