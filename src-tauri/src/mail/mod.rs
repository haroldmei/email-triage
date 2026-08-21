pub mod parser;

use std::{fmt::Display, future::Future, time::Duration};

use async_imap::Client;
use async_native_tls::TlsConnector;
use futures::TryStreamExt;
use thiserror::Error;
use tokio::{net::TcpStream, time::timeout};

use crate::models::{FetchedMessage, MailConfig};

const NETWORK_TIMEOUT: Duration = Duration::from_secs(15);
const IMAP_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum MailError {
    #[error("invalid mail configuration: {0}")]
    InvalidConfig(String),
    #[error("operation timed out: {0}")]
    Timeout(&'static str),
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
            return Err(MailError::InvalidConfig(
                "port must be greater than zero".into(),
            ));
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
    imap_op("select mailbox", session.select(&config.mailbox)).await?;
    imap_op("logout", session.logout()).await?;
    Ok(())
}

pub async fn fetch_candidate_messages(
    config: &MailConfig,
    password: &str,
    limit: usize,
) -> Result<Vec<FetchedMessage>, MailError> {
    config.validate()?;
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut session = connect(config, password).await?;
    imap_op("select mailbox", session.select(&config.mailbox)).await?;

    // Read/unread state belongs to the user's mail client and must not determine whether
    // Email Triage processes a message. Anything still in the configured source mailbox is
    // eligible; successfully handled messages are moved out to Processed or NeedsReview.
    let all = imap_op("search ALL", session.uid_search("ALL")).await?;
    let ids = select_latest_uids(all.into_iter().collect(), limit);

    let mut result = Vec::with_capacity(ids.len());
    for uid in ids {
        let raw = {
            // PEEK is intentional: merely reading a message must not set the IMAP \Seen flag.
            // A crash before Drive upload therefore leaves the message eligible for retry.
            let mut stream = imap_op(
                "start message fetch",
                session.uid_fetch(uid.to_string(), "BODY.PEEK[]"),
            )
            .await?;
            let fetch = imap_op("read message fetch", stream.try_next())
                .await?
                .ok_or_else(|| MailError::Imap(format!("message UID {uid} was not returned")))?;
            fetch
                .body()
                .map(ToOwned::to_owned)
                .ok_or_else(|| MailError::Imap(format!("message UID {uid} had no message body")))?
        };
        result.push(FetchedMessage { uid, raw });
    }

    imap_op("logout", session.logout()).await?;
    Ok(result)
}

fn select_latest_uids(mut ids: Vec<u32>, limit: usize) -> Vec<u32> {
    ids.sort_unstable();
    if ids.len() > limit {
        ids.split_off(ids.len() - limit)
    } else {
        ids
    }
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
    imap_op("select mailbox", session.select(&config.mailbox)).await?;

    // CREATE returning NO normally means the mailbox already exists. The move/copy below
    // is authoritative and will still fail if the mailbox is genuinely unavailable.
    let _ = timeout(IMAP_COMMAND_TIMEOUT, session.create(destination)).await;

    if imap_op("move message", session.uid_mv(uid.to_string(), destination))
        .await
        .is_err()
    {
        imap_op(
            "copy message",
            session.uid_copy(uid.to_string(), destination),
        )
        .await?;
        let updates = imap_op(
            "mark source message deleted",
            session.uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)"),
        )
        .await?;
        let _: Vec<_> = imap_op("collect store response", updates.try_collect()).await?;
        let expunged = imap_op("expunge deleted message", session.expunge()).await?;
        let _: Vec<_> = imap_op("collect expunge response", expunged.try_collect()).await?;
    }

    imap_op("logout", session.logout()).await?;
    Ok(())
}

async fn connect(
    config: &MailConfig,
    password: &str,
) -> Result<async_imap::Session<async_native_tls::TlsStream<TcpStream>>, MailError> {
    let address = format!("{}:{}", config.host, config.port);
    let tcp = timeout(NETWORK_TIMEOUT, TcpStream::connect(&address))
        .await
        .map_err(|_| MailError::Timeout("TCP connect"))?
        .map_err(|e| MailError::Network(e.to_string()))?;

    let tls = timeout(
        NETWORK_TIMEOUT,
        TlsConnector::new().connect(config.host.as_str(), tcp),
    )
    .await
    .map_err(|_| MailError::Timeout("TLS handshake"))?
    .map_err(|e| MailError::Tls(e.to_string()))?;

    let mut client = Client::new(tls);
    imap_op("server greeting", client.read_response())
        .await?
        .ok_or_else(|| MailError::Imap("server closed connection before greeting".into()))?;

    timeout(
        IMAP_COMMAND_TIMEOUT,
        client.login(config.username.as_str(), password),
    )
    .await
    .map_err(|_| MailError::Timeout("login"))?
    .map_err(|(e, _)| MailError::Imap(e.to_string()))
}

async fn imap_op<T, E, F>(stage: &'static str, future: F) -> Result<T, MailError>
where
    F: Future<Output = Result<T, E>>,
    E: Display,
{
    timeout(IMAP_COMMAND_TIMEOUT, future)
        .await
        .map_err(|_| MailError::Timeout(stage))?
        .map_err(|e| MailError::Imap(e.to_string()))
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
        assert!(matches!(
            config.validate(),
            Err(MailError::InvalidConfig(_))
        ));
    }

    #[test]
    fn selects_latest_uids_in_ascending_order() {
        assert_eq!(select_latest_uids(vec![8, 2, 10, 4], 3), vec![4, 8, 10]);
    }

    #[test]
    fn keeps_all_uids_when_under_limit() {
        assert_eq!(select_latest_uids(vec![3, 1, 2], 10), vec![1, 2, 3]);
    }
}