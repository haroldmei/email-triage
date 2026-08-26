from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"Expected application-filter patch anchor not found in {path}: {old[:140]!r}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


# The v0.1.20 patch already inserts `enrichment` before deterministic identity extraction.
# Make classification a hard safety gate before any Drive folder is matched or created.
replace_once(
    "src-tauri/src/workflow/mod.rs",
    '''    let student_name = preferred_student_name(&identity).map(|value| value.value.clone());
    let attachment_count = message.attachments.len();

    log_attachment_text_diagnostics(app, fetched.uid, &message.attachments);
''',
    '''    let student_name = preferred_student_name(&identity).map(|value| value.value.clone());
    let attachment_count = message.attachments.len();

    if !enrichment.classification_available {
        return review_result(
            fetched.uid,
            message.message_id.clone(),
            message.subject.clone(),
            None,
            attachment_count,
            "Local application-material classifier was unavailable; automatic filing was stopped safely".into(),
        );
    }

    if enrichment.relevant_filenames.is_empty() {
        logging::write(
            app,
            "INFO",
            format!(
                "stage=application_material_gate uid={} relevant_attachments=0 action=skip_message",
                fetched.uid
            ),
        );
        return ProcessingResult {
            uid: fetched.uid,
            message_id: message.message_id.clone(),
            subject: message.subject.clone(),
            student_name: None,
            folder_id: None,
            folder_name: None,
            attachment_count,
            uploaded_file_ids: Vec::new(),
            uploaded_file_names: Vec::new(),
            skipped_existing_files: Vec::new(),
            status: ProcessingStatus::ProcessedNoAttachments,
            detail: "No student university-application materials were found; message and attachments skipped".into(),
        };
    }

    log_attachment_text_diagnostics(app, fetched.uid, &message.attachments);
''',
)

# Only attachments classified as application-related are uploaded. Decorative/non-application
# attachments can never piggyback on a valid student's message.
replace_once(
    "src-tauri/src/workflow/mod.rs",
    '''    for (attachment_index, attachment) in message.attachments.iter().enumerate() {
''',
    '''    let application_attachments = message
        .attachments
        .iter()
        .filter(|attachment| {
            enrichment
                .relevant_filenames
                .iter()
                .any(|filename| filename == &attachment.filename)
        })
        .collect::<Vec<_>>();

    for (attachment_index, attachment) in application_attachments.iter().enumerate() {
''',
)

# Sequence denominator should describe upload-eligible application attachments, not every MIME part.
replace_once(
    "src-tauri/src/workflow/mod.rs",
    '''                attachment_index + 1,
                attachment_count,
                attachment.filename,
''',
    '''                attachment_index + 1,
                application_attachments.len(),
                attachment.filename,
''',
)

print("Applied application-material relevance gate v0.1.21")
