pub mod parser;

use std::{fmt::Display, future::Future, time::{Duration, Instant}};

use async_imap::Client;
use async_native_tls::TlsConnector;
use futures::TryStreamExt;
use tauri::AppHandle;
use thiserror::Error;
use tokio::{net::TcpStream, time::timeout};

use crate::{
    logging,
    models::{FetchedMessage, MailConfig},
};

const NETWORK_TIMEOUT: Duration = Duration::from_secs(15);
const IMAP_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
pub const MESSAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(120);
pub const RECENT_MESSAGE_WINDOW: usize = 50;

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

#[derive(Debug)]
pub struct MessageFetchFailure {
    pub uid: u32,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct MessageFetchBatch {
    pub messages: Vec<FetchedMessage>,
    pub failures: Vec<MessageFetchFailure>,
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

    // SEARCH ALL returns only UIDs, not message bodies. Keep only the highest UIDs,
    // which represent the most recently appended messages in this mailbox.
    let all = imap_op("search mailbox message UIDs", session.uid_search("ALL")).await?;
    let uids = recent_uids(all.into_iter().collect());

    imap_op("logout", session.logout()).await?;
    Ok(MailboxListing {
        uid_validity,
        total_messages: mailbox.exists,
        uids,
    })
}

fn recent_uids(mut uids: Vec<u32>) -> Vec<u32> {
    uids.sort_unstable();
    if uids.len() > RECENT_MESSAGE_WINDOW {
        uids.split_off(uids.len() - RECENT_MESSAGE_WINDOW)
    } else {
        uids
    }
}

pub async fn fetch_messages_by_uid(
    app: &AppHandle,
    config: &MailConfig,
    password: &str,
    expected_uid_validity: u32,
    uids: &[u32],
) -> Result<MessageFetchBatch, MailError> {
    config.validate()?;
    if uids.is_empty() {
        return Ok(MessageFetchBatch::default());
    }

    let mut session = connect_and_examine(config, password, expected_uid_validity).await?;
    let mut batch = MessageFetchBatch {
        messages: Vec::with_capacity(uids.len()),
        failures: Vec::new(),
    };

    for (index, uid) in uids.iter().enumerate() {
        let started = Instant::now();
        logging::write(
            app,
            "INFO",
            format!(
                "stage=message_fetch uid={uid} sequence={}/{} started timeout_seconds={}",
                index + 1,
                uids.len(),
                MESSAGE_FETCH_TIMEOUT.as_secs()
            ),
        );

        match fetch_one(&mut session, *uid).await {
            Ok(raw) => {
                logging::write(
                    app,
                    "INFO",
                    format!(
                        "stage=message_fetch uid={uid} sequence={}/{} completed bytes={} elapsed_ms={}",
                        index + 1,
                        uids.len(),
                        raw.len(),
                        started.elapsed().as_millis()
                    ),
                );
                batch.messages.push(FetchedMessage { uid: *uid, raw });
            }
            Err(error) => {
                let is_timeout = matches!(error, MailError::Timeout(_));
                let timeout_detail = if is_timeout {
                    format!(" timeout_seconds={}", MESSAGE_FETCH_TIMEOUT.as_secs())
                } else {
                    String::new()
                };
                logging::write(
                    app,
                    "ERROR",
                    format!(
                        "stage=message_fetch uid={uid} sequence={}/{} failed retryable=true elapsed_ms={}{} error=\"{}\"",
                        index + 1,
                        uids.len(),
                        started.elapsed().as_millis(),
                        timeout_detail,
                        error
                    ),
                );
                batch.failures.push(MessageFetchFailure {
                    uid: *uid,
                    error: error.to_string(),
                });

                // A timed-out/failed FETCH can leave the IMAP protocol stream mid-response.
                // Discard that session and reconnect before attempting the next UID.
                if index + 1 < uids.len() {
                    let reconnect_started = Instant::now();
                    logging::write(
                        app,
                        "INFO",
                        format!("stage=imap_reconnect after_uid={uid} started"),
                    );
                    session = connect_and_examine(config, password, expected_uid_validity).await?;
                    logging::write(
                        app,
                        "INFO",
                        format!(
                            "stage=imap_reconnect after_uid={uid} completed elapsed_ms={}",
                            reconnect_started.elapsed().as_millis()
                        ),
                    );
                }
            }
        }
    }

    let _ = imap_op("logout", session.logout()).await;
    Ok(batch)
}

async fn fetch_one(
    session: &mut async_imap::Session<async_native_tls::TlsStream<TcpStream>>,
    uid: u32,
) -> Result<Vec<u8>, MailError> {
    // BODY.PEEK[] is deliberate: it does not set the user's \\Seen flag.
    let mut stream = imap_op(
        "start message fetch",
        session.uid_fetch(uid.to_string(), "BODY.PEEK[]"),
    )
    .await?;
    let fetch = timeout(MESSAGE_FETCH_TIMEOUT, stream.try_next())
        .await
        .map_err(|_| MailError::Timeout("read message fetch"))?
        .map_err(|e| MailError::Imap(e.to_string()))?
        .ok_or_else(|| MailError::Imap(format!("message UID {uid} was not returned")))?;
    fetch
        .body()
        .map(ToOwned::to_owned)
        .ok_or_else(|| MailError::Imap(format!("message UID {uid} had no message body")))
}

async fn connect_and_examine(
    config: &MailConfig,
    password: &str,
    expected_uid_validity: u32,
) -> Result<async_imap::Session<async_native_tls::TlsStream<TcpStream>>, MailError> {
    let mut session = connect(config, password).await?;
    let mailbox = imap_op("examine mailbox", session.examine(&config.mailbox)).await?;
    if mailbox.uid_validity != Some(expected_uid_validity) {
        return Err(MailError::Imap(
            "mailbox UIDVALIDITY changed during processing; retry the mailbox check".into(),
        ));
    }
    Ok(session)
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
    fn keeps_only_the_highest_fifty_uids() {
        let selected = recent_uids((1..=60).rev().collect());
        assert_eq!(selected.len(), 50);
        assert_eq!(selected.first(), Some(&11));
        assert_eq!(selected.last(), Some(&60));
    }

    #[test]
    fn keeps_all_uids_when_mailbox_has_fewer_than_fifty() {
        assert_eq!(recent_uids(vec![3, 1, 2]), vec![1, 2, 3]);
    }

    #[test]
    fn message_fetch_timeout_is_longer_than_command_timeout() {
        assert!(MESSAGE_FETCH_TIMEOUT > IMAP_COMMAND_TIMEOUT);
        assert_eq!(MESSAGE_FETCH_TIMEOUT.as_secs(), 120);
    }
}
