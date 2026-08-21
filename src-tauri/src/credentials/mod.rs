use thiserror::Error;

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential was not found")]
    NotFound,
    #[error("credential store error: {0}")]
    Store(String),
}

/// Boundary for platform-backed secret storage.
///
/// Production implementations must use the Windows Credential Manager or
/// macOS Keychain. Secrets must never be persisted in application JSON config.
pub trait CredentialStore: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<String, CredentialError>;
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self, service: &str, account: &str) -> Result<(), CredentialError>;
}
