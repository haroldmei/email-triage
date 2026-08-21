# Security model

Email Triage is a local-first desktop application that handles student correspondence and attachments. The default is to minimise data movement and fail closed on identity ambiguity.

## Credentials

Mailbox credentials and Google refresh tokens are stored using the operating system credential store through the Rust `keyring` abstraction. Secrets must not be written to application JSON, source control, logs, crash reports, or GitHub Actions artifacts.

Google access tokens are short-lived and kept in process memory. The refresh token is retrieved from the platform credential store when a new access token is required.

## Student matching

Automatic upload requires one unique high-confidence folder match. Application/student identifiers are preferred over names. If a name maps to multiple folders, or the extractor cannot establish a sufficiently strong identity, the email is routed to the review mailbox without uploading any attachment.

## Attachments

MIME filenames are validated before use. Path separators, traversal filenames, and null-containing names are rejected. Drive uploads carry an application property containing a deterministic per-message attachment key. This allows retries after crashes or mail-move failures without creating duplicate files.

## Email state

The workflow uses stable IMAP UIDs within the selected mailbox. A message is moved to the Processed mailbox only after all required Drive work succeeds. Failures remain retryable. Ambiguous messages are moved to Needs Review.

## Network boundaries

The desktop app communicates directly with the configured mail server, Google OAuth endpoints, and Google Drive API over TLS. No central Email Triage backend is required for the MVP.

## Logging

Never log raw email bodies, attachment contents, mailbox passwords, OAuth tokens, or full student records. Operational errors should contain only enough metadata to diagnose the failure.
