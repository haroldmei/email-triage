# Release

## CI artifacts

`.github/workflows/release.yml` builds Windows and macOS bundles on version tags and by manual workflow dispatch. The build injects the optional `GOOGLE_OAUTH_CLIENT_ID` repository Actions variable as `VITE_GOOGLE_CLIENT_ID`, generates platform icon assets, builds the Tauri bundle, and uploads the bundle directory as a GitHub Actions artifact.

Unsigned CI artifacts are appropriate for internal engineering validation. Operating systems may warn users when installing unsigned applications.

## Google OAuth configuration

Set the GitHub Actions repository variable `GOOGLE_OAUTH_CLIENT_ID` to the Desktop OAuth client ID used by the distributed application. Do not store Google client secrets in the desktop application; native/desktop OAuth uses PKCE and the client ID is not a secret.

The current application needs to search existing Drive folders, so configure the Google OAuth consent screen and Workspace policies for the Drive scope used by the application. Complete any Google verification required before broad external distribution.

## Windows production signing

Use an Authenticode code-signing certificate from an appropriate certificate authority. Keep the certificate/private key in the release environment or GitHub encrypted secrets and integrate signing into the release workflow. Never commit signing material.

## macOS production signing and notarization

For broad macOS distribution, use an Apple Developer ID Application certificate and notarize the produced application/bundle with Apple. Store the certificate, password, Apple credentials/API key, and related signing values only in encrypted CI secrets.

## Versioning

Update the package/application version, merge the release-ready change to `main`, then create a tag such as `v0.1.0`. The tag triggers the cross-platform release build.

Before distributing to non-technical users, validate at least one real Tencent mailbox and one representative Google Workspace Drive hierarchy on both Windows and macOS.
