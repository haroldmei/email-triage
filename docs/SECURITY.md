# Security model

Email Triage is a local-first desktop application that handles student correspondence and attachments. The default is to minimise data movement and fail closed on identity ambiguity.

## Credentials

Mailbox credentials and Google refresh tokens are stored using the operating system credential store through the Rust `keyring` abstraction. Secrets must not be written to application JSON, source control, logs, crash reports, or GitHub Actions artifacts.

Google access tokens are short-lived and kept in process memory. The refresh token is retrieved from the platform credential store when a new access token is required.

## Student matching

Automatic upload requires one unique high-confidence folder match. Chinese student names are preferred when available; English is a fallback. If a name maps to multiple folders, or the extractor cannot establish a sufficiently strong identity, the message is recorded locally as **Needs Review** and no attachment is uploaded.

## Attachments

MIME filenames are validated before use. Path separators, traversal filenames, and null-containing names are rejected. Drive uploads carry an application property containing a deterministic per-message attachment key. This allows retries after crashes without creating duplicate files.

## Mail-server safety and processing state

Email Triage treats the configured mailbox as **read-only** for processing. It authenticates and opens the mailbox with IMAP `EXAMINE`, searches message UIDs, and fetches message content with `BODY.PEEK[]`. It does not create mail folders, move or copy messages, delete messages, expunge messages, set flags, or change read/unread state.

Read and unread messages are both eligible. Processing state is stored only on the local machine in `processing-state.json` under the Tauri application config directory. Entries are scoped by mail server, account, mailbox, IMAP `UIDVALIDITY`, and UID so a server-side UID reset cannot accidentally reuse stale local state. Successful, no-attachment, and needs-review outcomes are terminal locally; failures remain retryable.

## Network boundaries

The desktop app communicates directly with the configured mail server, Google OAuth endpoints, and Google Drive API over TLS. No central Email Triage backend is required for the MVP.

## Logging

Never log raw email bodies, attachment contents, mailbox passwords, OAuth tokens, or full student records. Operational errors should contain only enough metadata to diagnose the failure. Logs explicitly report that mail-server mutation is disabled.
