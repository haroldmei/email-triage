from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"Expected application-filter patch anchor not found in {path}: {old[:140]!r}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


# Make the local classifier a hard safety gate before any Drive folder is matched or created.
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
            detail: "No student-specific university-application materials were found; message and attachments skipped".into(),
        };
    }

    // v0.1.22: a relevant attachment is not enough to create a student folder. The ML worker must
    // have reached identity consensus from an explicit field or corroborating independent evidence.
    // Never fall back to broad subject/body PERSON heuristics for automatic filing.
    if enrichment.student_name.is_none() {
        logging::write(
            app,
            "WARN",
            format!(
                "stage=student_identity_gate uid={} relevant_attachments={} action=needs_review evidence=\"{}\"",
                fetched.uid,
                enrichment.relevant_filenames.len(),
                enrichment.evidence.replace('"', "'")
            ),
        );
        return review_result(
            fetched.uid,
            message.message_id.clone(),
            message.subject.clone(),
            None,
            attachment_count,
            format!("Student-specific application material was found, but no student identity reached safe consensus: {}", enrichment.evidence),
        );
    }

    log_attachment_text_diagnostics(app, fetched.uid, &message.attachments);
''',
)

# Only attachments classified as student-specific application material are uploaded.
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

print("Applied student-specific material and identity-consensus gate v0.1.22")
