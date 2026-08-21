# MVP Scope

## Goal

Reduce the manual workflow of opening a student-related email, determining the student, downloading attachments, finding the student's Google Drive folder, and uploading those files.

## In scope

1. Connect one Tencent Enterprise Email mailbox.
2. Fetch new messages directly over IMAP.
3. Parse subject, body, sender metadata, and attachments.
4. Extract a student name and supporting identifiers when available.
5. Find a matching student folder beneath one configured Google Drive root.
6. Upload attachments only when the match is sufficiently confident.
7. Mark/route successfully processed mail so it is not processed repeatedly.
8. Surface ambiguous messages for human review instead of guessing.
9. Package as a Windows/macOS Tauri desktop app.

## Explicitly out of scope for v0.1

- Reading Foxmail's local database or credentials.
- Generic discovery/automation of installed desktop software.
- A general-purpose workflow builder.
- Local MCP server.
- Full CRM/student-record system.
- Commission tracking.
- SQLite unless a concrete persistence requirement appears.

## Success criteria

- A representative test set can be processed end-to-end without manually downloading/uploading attachments.
- No attachment is intentionally auto-routed when the student match is ambiguous.
- Re-running processing does not create duplicate uploads for already completed messages under the chosen processing-state strategy.
- Setup is understandable from the app UI without a long manual.
