# Setup

Email Triage is designed to require only a short first-run setup.

## 1. Tencent Enterprise Email

Enter the mailbox address, IMAP server, TLS port, and the mailbox's client password or authorization code. The default server shown by the app is `imap.exmail.qq.com` on port `993`.

Some Tencent Enterprise Email tenants require IMAP access to be enabled by an administrator and may require a client authorization code instead of the normal interactive-login password. Confirm the current tenant policy if login is rejected.

The password/authorization code is stored through the operating system credential store. It is not written to `config.json`.

## 2. Google Workspace / Drive

A Google Cloud project must enable the Google Drive API and provide an OAuth 2.0 Desktop application client ID. For organisation-managed deployments, the client ID should normally be supplied to release builds through the `GOOGLE_OAUTH_CLIENT_ID` GitHub Actions variable so end users do not type it manually.

On first connection, Email Triage opens the system browser for Google consent, receives the desktop OAuth callback on loopback, and stores the refresh token in the operating system credential store.

The current MVP needs to discover pre-existing student folders, so it requests Drive access that is broader than `drive.file`. Configure the OAuth consent screen and any Workspace/admin restrictions appropriately before wider deployment.

After login, browse to the folder that contains all student folders and choose **Use this as student root**.

## 3. Automation

Choose the polling interval and optionally enable **Start automatically after login**. Closing the main window keeps the app running in the system tray.

Successfully processed messages are moved to `EmailTriage-Processed`. Messages without a unique, high-confidence student match are moved to `EmailTriage-NeedsReview` and no attachment is uploaded.

Use **Run now** during first setup to validate the workflow with a small number of representative emails before relying on background processing.
