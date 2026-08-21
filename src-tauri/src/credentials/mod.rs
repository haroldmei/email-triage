use keyring::Entry;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential store error: {0}")]
    Store(String),
}

pub trait CredentialStore: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<String, CredentialError>;
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self, service: &str, account: &str) -> Result<(), CredentialError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformCredentialStore;

impl PlatformCredentialStore {
    fn entry(service: &str, account: &str) -> Result<Entry, CredentialError> {
        Entry::new(service, account).map_err(|e| CredentialError::Store(e.to_string()))
    }
}

impl CredentialStore for PlatformCredentialStore {
    fn get(&self, service: &str, account: &str) -> Result<String, CredentialError> {
        Self::entry(service, account)?
            .get_password()
            .map_err(|e| CredentialError::Store(e.to_string()))
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), CredentialError> {
        Self::entry(service, account)?
            .set_password(secret)
            .map_err(|e| CredentialError::Store(e.to_string()))
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), CredentialError> {
        Self::entry(service, account)?
            .delete_credential()
            .map_err(|e| CredentialError::Store(e.to_string()))
    }
}

pub const MAIL_SERVICE: &str = "email-triage/tencent-mail";
pub const GOOGLE_SERVICE: &str = "email-triage/google-drive";
