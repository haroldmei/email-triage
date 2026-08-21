use std::time::Duration;

use oauth2::{
    basic::BasicClient, AuthorizationCode, AuthUrl, ClientId, CsrfToken, PkceCodeChallenge,
    RedirectUrl, RefreshToken, Scope, TokenResponse, TokenUrl,
};
use reqwest::{header, Client as HttpClient};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use url::Url;

use crate::{
    credentials::{CredentialStore, PlatformCredentialStore, GOOGLE_SERVICE},
    models::Attachment,
};

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive";
const DRIVE_API: &str = "https://www.googleapis.com/drive/v3";
const DRIVE_UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3";

#[derive(Debug, Error)]
pub enum GoogleDriveError {
    #[error("OAuth configuration error: {0}")]
    OAuthConfig(String),
    #[error("OAuth callback timed out")]
    OAuthTimeout,
    #[error("OAuth state mismatch")]
    OAuthStateMismatch,
    #[error("OAuth callback error: {0}")]
    OAuthCallback(String),
    #[error("OAuth token error: {0}")]
    OAuthToken(String),
    #[error("browser launch failed: {0}")]
    Browser(String),
    #[error("credential store error: {0}")]
    Credential(String),
    #[error("Google Drive API error: {0}")]
    Api(String),
    #[error("Google Drive response was missing {0}")]
    MissingResponseField(&'static str),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GoogleConnection {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    pub parents: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct DriveFileList {
    #[serde(default)]
    files: Vec<DriveFile>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DriveAbout {
    user: DriveUser,
}

#[derive(Debug, Deserialize)]
struct DriveUser {
    #[serde(rename = "emailAddress")]
    email_address: String,
}

pub async fn connect_google(client_id: &str) -> Result<GoogleConnection, GoogleDriveError> {
    if client_id.trim().is_empty() {
        return Err(GoogleDriveError::OAuthConfig(
            "Google desktop OAuth client ID is required".into(),
        ));
    }

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| GoogleDriveError::OAuthCallback(e.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|e| GoogleDriveError::OAuthCallback(e.to_string()))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}/oauth/callback");

    let client = oauth_client(client_id, &redirect)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, csrf) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(DRIVE_SCOPE.to_string()))
        .add_extra_param("access_type", "offline")
        .add_extra_param("prompt", "consent")
        .set_pkce_challenge(challenge)
        .url();

    open::that(auth_url.as_str()).map_err(|e| GoogleDriveError::Browser(e.to_string()))?;

    let (code, returned_state) = receive_oauth_callback(listener).await?;
    if returned_state != *csrf.secret() {
        return Err(GoogleDriveError::OAuthStateMismatch);
    }

    let http = oauth_http_client()?;
    let token = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(verifier)
        .request_async(&http)
        .await
        .map_err(|e| GoogleDriveError::OAuthToken(e.to_string()))?;

    let access_token = token.access_token().secret().to_string();
    let refresh_token = token
        .refresh_token()
        .ok_or_else(|| GoogleDriveError::OAuthToken("Google did not return a refresh token".into()))?
        .secret()
        .to_string();

    let drive = DriveClient::new(access_token);
    let email = drive.current_user_email().await?;
    PlatformCredentialStore
        .set(GOOGLE_SERVICE, &email, &refresh_token)
        .map_err(|e| GoogleDriveError::Credential(e.to_string()))?;

    Ok(GoogleConnection { email })
}

pub async fn client_from_stored_refresh_token(
    client_id: &str,
    email: &str,
) -> Result<DriveClient, GoogleDriveError> {
    let refresh = PlatformCredentialStore
        .get(GOOGLE_SERVICE, email)
        .map_err(|e| GoogleDriveError::Credential(e.to_string()))?;
    let client = oauth_client(client_id, "http://127.0.0.1")?;
    let http = oauth_http_client()?;
    let token = client
        .exchange_refresh_token(&RefreshToken::new(refresh))
        .request_async(&http)
        .await
        .map_err(|e| GoogleDriveError::OAuthToken(e.to_string()))?;
    Ok(DriveClient::new(token.access_token().secret().to_string()))
}

fn oauth_client(
    client_id: &str,
    redirect_uri: &str,
) -> Result<
    BasicClient<
        oauth2::EndpointSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointSet,
    >,
    GoogleDriveError,
> {
    let auth = AuthUrl::new(GOOGLE_AUTH_URL.to_string())
        .map_err(|e| GoogleDriveError::OAuthConfig(e.to_string()))?;
    let token = TokenUrl::new(GOOGLE_TOKEN_URL.to_string())
        .map_err(|e| GoogleDriveError::OAuthConfig(e.to_string()))?;
    let redirect = RedirectUrl::new(redirect_uri.to_string())
        .map_err(|e| GoogleDriveError::OAuthConfig(e.to_string()))?;
    Ok(BasicClient::new(ClientId::new(client_id.to_string()))
        .set_auth_uri(auth)
        .set_token_uri(token)
        .set_redirect_uri(redirect))
}

fn oauth_http_client() -> Result<HttpClient, GoogleDriveError> {
    HttpClient::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| GoogleDriveError::OAuthConfig(e.to_string()))
}

async fn receive_oauth_callback(listener: TcpListener) -> Result<(String, String), GoogleDriveError> {
    let (mut socket, _) = timeout(Duration::from_secs(180), listener.accept())
        .await
        .map_err(|_| GoogleDriveError::OAuthTimeout)?
        .map_err(|e| GoogleDriveError::OAuthCallback(e.to_string()))?;

    let mut buffer = vec![0_u8; 8192];
    let count = timeout(Duration::from_secs(10), socket.read(&mut buffer))
        .await
        .map_err(|_| GoogleDriveError::OAuthTimeout)?
        .map_err(|e| GoogleDriveError::OAuthCallback(e.to_string()))?;
    let request = String::from_utf8_lossy(&buffer[..count]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| GoogleDriveError::OAuthCallback("invalid HTTP callback".into()))?;
    let callback = Url::parse(&format!("http://127.0.0.1{path}"))
        .map_err(|e| GoogleDriveError::OAuthCallback(e.to_string()))?;

    let mut code = None;
    let mut state = None;
    let mut oauth_error = None;
    for (key, value) in callback.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => oauth_error = Some(value.into_owned()),
            _ => {}
        }
    }

    let success = oauth_error.is_none() && code.is_some() && state.is_some();
    let body = if success {
        "Email Triage is connected to Google Drive. You can close this browser tab."
    } else {
        "Email Triage could not complete Google authorization. Return to the app for details."
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = socket.write_all(response.as_bytes()).await;

    if let Some(error) = oauth_error {
        return Err(GoogleDriveError::OAuthCallback(error));
    }
    Ok((
        code.ok_or_else(|| GoogleDriveError::OAuthCallback("missing authorization code".into()))?,
        state.ok_or_else(|| GoogleDriveError::OAuthCallback("missing OAuth state".into()))?,
    ))
}

#[derive(Clone)]
pub struct DriveClient {
    http: HttpClient,
    access_token: String,
}

impl DriveClient {
    pub fn new(access_token: String) -> Self {
        Self {
            http: HttpClient::new(),
            access_token,
        }
    }

    async fn current_user_email(&self) -> Result<String, GoogleDriveError> {
        let response = self
            .http
            .get(format!("{DRIVE_API}/about"))
            .bearer_auth(&self.access_token)
            .query(&[("fields", "user(emailAddress)")])
            .send()
            .await
            .map_err(|e| GoogleDriveError::Api(e.to_string()))?;
        let response = ensure_success(response).await?;
        let about: DriveAbout = response
            .json()
            .await
            .map_err(|e| GoogleDriveError::Api(e.to_string()))?;
        Ok(about.user.email_address)
    }

    pub async fn list_folders(&self, parent_id: &str) -> Result<Vec<DriveFile>, GoogleDriveError> {
        let escaped_parent = parent_id.replace('\'', "\\'");
        let query = format!(
            "'{escaped_parent}' in parents and mimeType='application/vnd.google-apps.folder' and trashed=false"
        );
        let mut page_token: Option<String> = None;
        let mut folders = Vec::new();

        loop {
            let mut request = self
                .http
                .get(format!("{DRIVE_API}/files"))
                .bearer_auth(&self.access_token)
                .query(&[
                    ("q", query.as_str()),
                    ("fields", "nextPageToken,files(id,name,mimeType,parents)"),
                    ("pageSize", "1000"),
                    ("supportsAllDrives", "true"),
                    ("includeItemsFromAllDrives", "true"),
                ]);
            if let Some(token) = &page_token {
                request = request.query(&[("pageToken", token.as_str())]);
            }
            let response = request
                .send()
                .await
                .map_err(|e| GoogleDriveError::Api(e.to_string()))?;
            let response = ensure_success(response).await?;
            let page: DriveFileList = response
                .json()
                .await
                .map_err(|e| GoogleDriveError::Api(e.to_string()))?;
            folders.extend(page.files);
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(folders)
    }

    pub async fn find_files_named(
        &self,
        parent_id: &str,
        filename: &str,
    ) -> Result<Vec<DriveFile>, GoogleDriveError> {
        let parent = parent_id.replace('\'', "\\'");
        let name = filename.replace('\'', "\\'");
        let query = format!("'{parent}' in parents and name='{name}' and trashed=false");
        let response = self
            .http
            .get(format!("{DRIVE_API}/files"))
            .bearer_auth(&self.access_token)
            .query(&[
                ("q", query.as_str()),
                ("fields", "files(id,name,mimeType,parents)"),
                ("supportsAllDrives", "true"),
                ("includeItemsFromAllDrives", "true"),
            ])
            .send()
            .await
            .map_err(|e| GoogleDriveError::Api(e.to_string()))?;
        let response = ensure_success(response).await?;
        let page: DriveFileList = response
            .json()
            .await
            .map_err(|e| GoogleDriveError::Api(e.to_string()))?;
        Ok(page.files)
    }

    pub async fn upload_attachment(
        &self,
        folder_id: &str,
        attachment: &Attachment,
    ) -> Result<DriveFile, GoogleDriveError> {
        let metadata = serde_json::json!({
            "name": attachment.filename,
            "parents": [folder_id]
        });
        let initiation = self
            .http
            .post(format!("{DRIVE_UPLOAD_API}/files"))
            .bearer_auth(&self.access_token)
            .query(&[("uploadType", "resumable"), ("supportsAllDrives", "true")])
            .header("X-Upload-Content-Type", &attachment.content_type)
            .header("X-Upload-Content-Length", attachment.bytes.len())
            .json(&metadata)
            .send()
            .await
            .map_err(|e| GoogleDriveError::Api(e.to_string()))?;
        let initiation = ensure_success(initiation).await?;
        let location = initiation
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(GoogleDriveError::MissingResponseField("resumable upload Location header"))?
            .to_string();

        let upload = self
            .http
            .put(location)
            .bearer_auth(&self.access_token)
            .header(header::CONTENT_TYPE, &attachment.content_type)
            .header(header::CONTENT_LENGTH, attachment.bytes.len())
            .body(attachment.bytes.clone())
            .send()
            .await
            .map_err(|e| GoogleDriveError::Api(e.to_string()))?;
        let upload = ensure_success(upload).await?;
        upload
            .json::<DriveFile>()
            .await
            .map_err(|e| GoogleDriveError::Api(e.to_string()))
    }
}

async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response, GoogleDriveError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    Err(GoogleDriveError::Api(format!("HTTP {status}: {text}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_file_deserializes_google_shape() {
        let file: DriveFile = serde_json::from_str(
            r#"{"id":"1","name":"Chang Rui","mimeType":"application/vnd.google-apps.folder","parents":["root"]}"#,
        )
        .unwrap();
        assert_eq!(file.id, "1");
        assert_eq!(file.name, "Chang Rui");
    }
}
