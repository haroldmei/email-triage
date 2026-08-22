pub mod parser;

use std::{fmt::Display, future::Future, time::Duration};

use async_imap::Client;
use async_native_tls::TlsConnector;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::TryStreamExt;
use thiserror::Error;
use tokio::{net::TcpStream, time::timeout};

use crate::models::{FetchedMessage, MailConfig};

const NETWORK_TIMEOUT: Duration = Duration::from_secs(15);
const IMAP_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAIL_LOOKBACK_HOURS: i64 = 12;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxListing {
    pub uid_validity: u32,
    pub total_messages: u32,
    pub uids: Vec<u32>,
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
    imap_op("examine mailbox", session.examine(&config.mailbox)).await?;
    imap_op("logout", session.logout()).await?;
    Ok(())
}

pub async fn list_message_uids(
    config: &MailConfig,
    password: &str,
) -> Result<MailboxListing, MailError> {
    config.validate()?;
    let mut session = connect(config, password).await?;
    let mailbox = imap_op("examine mailbox", session.examine(&config.mailbox)).await?;
    let uid_validity = mailbox.uid_validity.ok_or_else(|| {
        MailError::Imap("examined mailbox did not report UIDVALIDITY".into())
    })?;

    let cutoff = Utc::now() - ChronoDuration::hours(MAIL_LOOKBACK_HOURS);

    // IMAP SINCE has day-level precision, so use it only to narrow the server-side search.
    // INTERNALDATE below applies the exact rolling 12-hour cutoff before any full body is fetched.
    let since_query = format!("SINCE {}", cutoff.format("%d-%b-%Y"));
    let coarse = imap_op("search recent messages", session.uid_search(since_query)).await?;
    let mut coarse_uids: Vec<u32> = coarse.into_iter().collect();
    coarse_uids.sort_unstable();

    let mut recent_uids = Vec::new();
    if !coarse_uids.is_empty() {
        let uid_set = coarse_uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut stream = imap_op(
            "start recent metadata fetch",
            session.uid_fetch(uid_set, "(UID INTERNALDATE)"),
        )
        .await?;

        while let Some(fetch) = imap_op("read recent metadata fetch", stream.try_next()).await? {
            let Some(uid) = fetch.uid else {
                continue;
            };
            let Some(internal_date) = fetch.internal_date() else {
                continue;
            };
            if is_within_lookback(internal_date.with_timezone(&Utc), cutoff) {
                recent_uids.push(uid);
            }
        }
    }
    recent_uids.sort_unstable();

    imap_op("logout", session.logout()).await?;
    Ok(MailboxListing {
        uid_validity,
        total_messages: mailbox.exists,
        uids: recent_uids,
    })
}

pub async fn fetch_messages_by_uid(
    config: &MailConfig,
    password: &str,
    expected_uid_validity: u32,
    uids: &[u32],
) -> Result<Vec<FetchedMessage>, MailError> {
    config.validate()?;
    if uids.is_empty() {
        return Ok(Vec::new());
    }

    let mut session = connect(config, password).await?;
    let mailbox = imap_op("examine mailbox", session.examine(&config.mailbox)).await?;
    if mailbox.uid_validity != Some(expected_uid_validity) {
        return Err(MailError::Imap(
            "mailbox UIDVALIDITY changed during processing; retry the mailbox check".into(),
        ));
    }

    let mut result = Vec::with_capacity(uids.len());
    for uid in uids {
        let raw = {
            // BODY.PEEK[] is deliberate: it does not set the user's \\Seen flag.
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
        result.push(FetchedMessage { uid: *uid, raw });
    }

    imap_op("logout", session.logout()).await?;
    Ok(result)
}

fn is_within_lookback(internal_date: DateTime<Utc>, cutoff: DateTime<Utc>) -> bool {
    internal_date >= cutoff
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
    fn exact_lookback_includes_boundary_and_excludes_older_mail() {
        let cutoff = DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let boundary = DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let older = DateTime::parse_from_rfc3339("2026-08-21T11:59:59Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(is_within_lookback(boundary, cutoff));
        assert!(!is_within_lookback(older, cutoff));
    }
}
