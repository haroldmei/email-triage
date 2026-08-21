# Architecture

## Product boundary

Email Triage is a local-first desktop application. Foxmail may remain installed for human use, but automation connects directly to Tencent Enterprise Email rather than automating Foxmail UI.

## MVP data flow

```text
Tencent Enterprise Email
        |
        | IMAP
        v
Mail connector
        |
        v
MIME parser
        |
        +--> subject/body
        +--> attachments
        |
        v
Student-name extraction
        |
        v
Google Drive folder match
        |
        v
Upload attachments
        |
        v
Mark message processed / route ambiguous cases for review
```

## Components

### React UI
- First-run onboarding
- Connection status
- Drive root selection
- Processing status and review path

### Tauri/Rust core
- IMAP connection and message retrieval
- MIME parsing
- Deterministic extraction and matching
- LLM fallback when deterministic extraction is insufficient
- Google OAuth and Drive API client
- Scheduling and retries
- OS credential-store integration

## State strategy for v0.1

No SQLite. Prefer mail-server state (for example a dedicated processed folder or IMAP state) plus minimal local configuration. Add a durable local database only when requirements such as a retry queue, processing history, audit trail, or cached mapping justify it.

## Security principles

- Never read or copy Foxmail's stored credentials.
- Never store mailbox passwords or OAuth refresh tokens in plain-text config files.
- Use Windows Credential Manager / macOS Keychain for secrets.
- Use least-privilege Google OAuth scopes.
- Never put an LLM provider API key in the desktop bundle; use a backend gateway or user-supplied secret stored in the OS credential store.
- Do not auto-upload when student-folder matching is ambiguous.

## Initial module boundaries

```text
src-tauri/src/
  mail/          IMAP + MIME
  extraction/    name/entity extraction
  google_drive/  OAuth + Drive operations
  workflow/      orchestration, retries, routing
  credentials/   OS secure storage
```

These modules will be introduced incrementally as their issues are implemented.
