pub mod parser;

use std::time::Duration;

use async_imap::Client;
use async_native_tls::TlsConnector;
use futures::TryStreamExt;
use thiserror::Error;
use tokio::{net::TcpStream, time::timeout};

use crate::models::{FetchedMessage, MailConfig};

#[derive(Debug, Error)]
pub enum MailError {
    #[error("invalid mail configuration: {0}")]
    InvalidConfig(String),
    #[error("connection timed out")]
    Timeout,
    #[error("network error: {0}")]
    Network(String),
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("IMAP error: {0}")]
    Imap(String),
}

impl MailConfig {
    pub fn validate(&self) -> Result<(), MailError> {
        if self.host.trim().is_empty() {
            return Err(MailError::InvalidConfig("host is required".into()));
        }
        if self.username.trim().is_empty() {
            return Err(MailError::InvalidConfig("username is required".into()));
        }
        if self.port == 0 {
            return Err(MailError::InvalidConfig("port must be greater than zero".into()));
        }
        if self.mailbox.trim().is_empty() {
            return Err(MailError::InvalidConfig("mailbox is required".into()));
        }
        Ok(())
    }
}

pub async fn validate_connection(config: &MailConfig, password: &str) -> Result<(), MailError> {
    config.validate()?;
    let mut session = connect(config, password).await?;
    session
        .select(&config.mailbox)
        .await
        .map_err(|e| MailError::Imap(e.to_string()))?;
    session
        .logout()
        .await
        .map_err(|e| MailError::Imap(e.to_string()))?;
    Ok(())
}

pub async fn fetch_unseen_messages(
    config: &MailConfig,
    password: &str,
    limit: usize,
) -> Result<Vec<FetchedMessage>, MailError> {
    config.validate()?;
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut session = connect(config, password).await?;
    session
        .select(&config.mailbox)
        .await
        .map_err(|e| MailError::Imap(e.to_string()))?;

    let unseen = session
        .uid_search("UNSEEN")
        .await
        .map_err(|e| MailError::Imap(e.to_string()))?;

    let mut ids: Vec<u32> = unseen.into_iter().collect();
    ids.sort_unstable();
    if ids.len() > limit {
        ids = ids.split_off(ids.len() - limit);
    }

    let mut result = Vec::with_capacity(ids.len());
    for uid in ids {
        let raw = {
            // PEEK is intentional: merely reading a message must not set the IMAP \\Seen flag.
            // A crash before Drive upload should therefore leave the message eligible for retry.
            let mut stream = session
                .uid_fetch(uid.to_string(), "BODY.PEEK[]")
                .await
                .map_err(|e| MailError::Imap(e.to_string()))?;
            let fetch = stream
                .try_next()
                .await
                .map_err(|e| MailError::Imap(e.to_string()))?
                .ok_or_else(|| MailError::Imap(format!("message UID {uid} was not returned")))?;
            fetch
                .body()
                .map(ToOwned::to_owned)
                .ok_or_else(|| MailError::Imap(format!("message UID {uid} had no message body")))?
        };
        result.push(FetchedMessage { uid, raw });
    }

    session
        .logout()
        .await
        .map_err(|e| MailError::Imap(e.to_string()))?;
    Ok(result)
}

pub async fn move_message(
    config: &MailConfig,
    password: &str,
    uid: u32,
    destination: &str,
) -> Result<(), MailError> {
    if destination.trim().is_empty() {
        return Err(MailError::InvalidConfig(
            "destination mailbox is required".into(),
        ));
    }

    let mut session = connect(config, password).await?;
    session
        .select(&config.mailbox)
        .await
        .map_err(|e| MailError::Imap(e.to_string()))?;

    // CREATE returning NO normally means the mailbox already exists. The move/copy below
    // is authoritative and will still fail if the mailbox is genuinely unavailable.
    let _ = session.create(destination).await;

    if session.uid_mv(uid.to_string(), destination).await.is_err() {
        session
            .uid_copy(uid.to_string(), destination)
            .await
            .map_err(|e| MailError::Imap(e.to_string()))?;
        let updates = session
            .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
            .await
            .map_err(|e| MailError::Imap(e.to_string()))?;
        let _: Vec<_> = updates
            .try_collect()
            .await
            .map_err(|e| MailError::Imap(e.to_string()))?;
        session
            .expunge()
            .await
            .map_err(|e| MailError::Imap(e.to_string()))?;
    }

    session
        .logout()
        .await
        .map_err(|e| MailError::Imap(e.to_string()))?;
    Ok(())
}

async fn connect(
    config: &MailConfig,
    password: &str,
) -> Result<async_imap::Session<async_native_tls::TlsStream<TcpStream>>, MailError> {
    let address = format!("{}:{}", config.host, config.port);
    let tcp = timeout(Duration::from_secs(15), TcpStream::connect(&address))
        .await
        .map_err(|_| MailError::Timeout)?
        .map_err(|e| MailError::Network(e.to_string()))?;

    let tls = timeout(
        Duration::from_secs(15),
        TlsConnector::new().connect(config.host.as_str(), tcp),
    )
    .await
    .map_err(|_| MailError::Timeout)?
    .map_err(|e| MailError::Tls(e.to_string()))?;

    let mut client = Client::new(tls);
    client
        .read_response()
        .await
        .map_err(|e| MailError::Imap(e.to_string()))?
        .ok_or_else(|| MailError::Imap("server closed connection before greeting".into()))?;

    client
        .login(config.username.as_str(), password)
        .await
        .map_err(|(e, _)| MailError::Imap(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_configuration() {
        let config = MailConfig {
            host: "".into(),
            port: 993,
            username: "user@example.com".into(),
            mailbox: "INBOX".into(),
        };
        assert!(matches!(config.validate(), Err(MailError::InvalidConfig(_))));
    }
}
