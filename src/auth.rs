use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::Profile;
use crate::error::AppError;
use crate::output::Output;

pub const BASE_SCOPES: &str = "openid profile offline_access User.Read Team.ReadBasic.All Channel.ReadBasic.All Chat.Read Chat.Create ChatMessage.Send ChannelMessage.Send";
pub const CHANNEL_HISTORY_SCOPE: &str = "ChannelMessage.Read.All";
pub const CHANNEL_HISTORY_SCOPES: &str = "openid profile offline_access User.Read Team.ReadBasic.All Channel.ReadBasic.All Chat.Read Chat.Create ChatMessage.Send ChannelMessage.Send ChannelMessage.Read.All";
const SERVICE: &str = "teams-cli";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBundle {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
    error_description: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn entry(profile_name: &str) -> Result<keyring::Entry, AppError> {
    keyring::Entry::new(SERVICE, profile_name)
        .map_err(|e| AppError::Unexpected(format!("credential store is unavailable: {e}")))
}

pub fn has_token(profile_name: &str) -> bool {
    std::env::var("TEAMS_ACCESS_TOKEN").is_ok()
        || entry(profile_name)
            .and_then(|e| {
                e.get_password()
                    .map_err(|err| AppError::Unexpected(err.to_string()))
            })
            .is_ok()
}

pub fn granted_scopes(profile_name: &str) -> Option<Vec<String>> {
    if std::env::var("TEAMS_ACCESS_TOKEN").is_ok() {
        return None;
    }
    load(profile_name).ok().and_then(|token| {
        token.scope.map(|scope| {
            scope
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
    })
}

pub fn channel_history_granted(profile_name: &str) -> Option<bool> {
    granted_scopes(profile_name).map(|scopes| {
        scopes
            .iter()
            .any(|scope| scope.eq_ignore_ascii_case(CHANNEL_HISTORY_SCOPE))
    })
}

fn store(profile_name: &str, token: &TokenBundle) -> Result<(), AppError> {
    let encoded = serde_json::to_string(token).map_err(|e| AppError::Unexpected(e.to_string()))?;
    entry(profile_name)?.set_password(&encoded).map_err(|e| {
        AppError::Unexpected(format!(
            "could not save credentials in the OS credential store: {e}"
        ))
    })
}

fn load(profile_name: &str) -> Result<TokenBundle, AppError> {
    let encoded = entry(profile_name)?
        .get_password()
        .map_err(|_| AppError::Auth("not signed in; run `teams auth login`".into()))?;
    serde_json::from_str(&encoded).map_err(|_| {
        AppError::Auth("stored credential is unreadable; run `teams auth login` again".into())
    })
}

pub fn logout(profile_name: &str) -> Result<(), AppError> {
    match entry(profile_name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AppError::Unexpected(format!(
            "could not remove stored credentials: {e}"
        ))),
    }
}

pub async fn access_token(profile_name: &str, profile: &Profile) -> Result<String, AppError> {
    if let Ok(token) = std::env::var("TEAMS_ACCESS_TOKEN") {
        return Ok(token);
    }
    let mut bundle = load(profile_name)?;
    if bundle.expires_at > now() + 120 {
        return Ok(bundle.access_token);
    }
    let refresh = bundle.refresh_token.as_deref().ok_or_else(|| {
        AppError::Auth(
            "session expired and has no refresh credential; run `teams auth login`".into(),
        )
    })?;
    let response = reqwest::Client::new()
        .post(token_url(profile))
        .form(&[
            ("client_id", profile.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("scope", bundle.scope.as_deref().unwrap_or(BASE_SCOPES)),
        ])
        .send()
        .await
        .map_err(|e| AppError::Unexpected(format!("sign-in service is unreachable: {e}")))?;
    let mut replacement = parse_token_response(response).await?;
    if replacement.refresh_token.is_none() {
        replacement.refresh_token = bundle.refresh_token.clone();
    }
    bundle = replacement;
    store(profile_name, &bundle)?;
    Ok(bundle.access_token)
}

pub async fn login(
    profile_name: &str,
    profile: &Profile,
    device_code: bool,
    channel_history: bool,
    out: &Output,
) -> Result<TokenBundle, AppError> {
    let scopes = scopes(channel_history);
    let bundle = if device_code {
        device_login(profile, scopes, out).await?
    } else {
        browser_login(profile, scopes, out).await?
    };
    store(profile_name, &bundle)?;
    Ok(bundle)
}

fn scopes(channel_history: bool) -> &'static str {
    if channel_history {
        CHANNEL_HISTORY_SCOPES
    } else {
        BASE_SCOPES
    }
}

async fn browser_login(
    profile: &Profile,
    scopes: &str,
    out: &Output,
) -> Result<TokenBundle, AppError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let redirect = format!("http://localhost:{}", listener.local_addr()?.port());
    let mut verifier_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut verifier_bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut state_bytes = [0u8; 18];
    rand::rng().fill_bytes(&mut state_bytes);
    let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state_bytes);
    let mut authorize = url::Url::parse(&format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
        profile.tenant
    ))
    .map_err(|e| AppError::Unexpected(e.to_string()))?;
    authorize
        .query_pairs_mut()
        .append_pair("client_id", &profile.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect)
        .append_pair("response_mode", "query")
        .append_pair("scope", scopes)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);
    out.note("Opening Microsoft sign-in in your browser…");
    out.note(format!(
        "If it does not open, visit:\n{}",
        authorize.as_str()
    ));
    open::that(authorize.as_str())
        .map_err(|e| AppError::Unexpected(format!("could not open a browser: {e}")))?;

    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(300), listener.accept())
        .await
        .map_err(|_| AppError::Auth("browser sign-in timed out after 5 minutes".into()))??;
    let mut bytes = vec![0; 8192];
    let count = socket.read(&mut bytes).await?;
    let request = String::from_utf8_lossy(&bytes[..count]);
    let target = request
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| AppError::Auth("invalid browser callback".into()))?;
    let callback = url::Url::parse(&format!("http://localhost{target}"))
        .map_err(|e| AppError::Auth(e.to_string()))?;
    let params: HashMap<_, _> = callback.query_pairs().into_owned().collect();
    let ok = params.contains_key("code");
    let page = if ok {
        "Sign-in complete. You can close this tab and return to teams."
    } else {
        "Sign-in did not complete. Return to the terminal for details."
    };
    let body = format!(
        "<html><body style=\"font-family:system-ui;max-width:42rem;margin:10vh auto;padding:2rem\"><h1>{page}</h1></body></html>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    if params.get("state") != Some(&state) {
        return Err(AppError::Auth(
            "sign-in callback state did not match".into(),
        ));
    }
    if let Some(error) = params
        .get("error_description")
        .or_else(|| params.get("error"))
    {
        return Err(AppError::Auth(error.clone()));
    }
    let code = params
        .get("code")
        .ok_or_else(|| AppError::Auth("Microsoft did not return an authorization code".into()))?;
    let response = reqwest::Client::new()
        .post(token_url(profile))
        .form(&[
            ("client_id", profile.client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", verifier.as_str()),
            ("scope", scopes),
        ])
        .send()
        .await
        .map_err(|e| AppError::Unexpected(format!("sign-in service is unreachable: {e}")))?;
    parse_token_response(response).await
}

#[derive(Debug, Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: Option<u64>,
    message: Option<String>,
}

async fn device_login(
    profile: &Profile,
    scopes: &str,
    out: &Output,
) -> Result<TokenBundle, AppError> {
    let response = reqwest::Client::new()
        .post(format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
            profile.tenant
        ))
        .form(&[("client_id", profile.client_id.as_str()), ("scope", scopes)])
        .send()
        .await
        .map_err(|e| AppError::Unexpected(format!("sign-in service is unreachable: {e}")))?;
    if !response.status().is_success() {
        return Err(oauth_response_error(response).await);
    }
    let device: DeviceCode = response
        .json()
        .await
        .map_err(|e| AppError::Auth(format!("invalid device-code response: {e}")))?;
    out.note(device.message.clone().unwrap_or_else(|| {
        format!(
            "Open {} and enter {}",
            device.verification_uri, device.user_code
        )
    }));
    let deadline = now() + device.expires_in;
    let interval = device.interval.unwrap_or(5);
    while now() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let response = reqwest::Client::new()
            .post(token_url(profile))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", profile.client_id.as_str()),
                ("device_code", device.device_code.as_str()),
            ])
            .send()
            .await
            .map_err(|e| AppError::Unexpected(e.to_string()))?;
        if response.status().is_success() {
            return parse_token_response(response).await;
        }
        let status = response.status();
        let error: OAuthError = response.json().await.unwrap_or(OAuthError {
            error: "unknown_error".into(),
            error_description: None,
        });
        if error.error == "authorization_pending" {
            continue;
        }
        if error.error == "slow_down" {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        return Err(AppError::Auth(
            error
                .error_description
                .unwrap_or_else(|| format!("{} ({status})", error.error)),
        ));
    }
    Err(AppError::Auth("device-code sign-in expired".into()))
}

fn token_url(profile: &Profile) -> String {
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        profile.tenant
    )
}

async fn parse_token_response(response: reqwest::Response) -> Result<TokenBundle, AppError> {
    if !response.status().is_success() {
        return Err(oauth_response_error(response).await);
    }
    let token: TokenResponse = response
        .json()
        .await
        .map_err(|e| AppError::Auth(format!("invalid token response: {e}")))?;
    Ok(TokenBundle {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: now() + token.expires_in,
        scope: token.scope,
    })
}

async fn oauth_response_error(response: reqwest::Response) -> AppError {
    let status = response.status();
    let error: Option<OAuthError> = response.json().await.ok();
    AppError::Auth(
        error
            .and_then(|e| e.error_description.or(Some(e.error)))
            .unwrap_or_else(|| format!("Microsoft sign-in failed with {status}")),
    )
}

#[cfg(test)]
mod tests {
    use super::{BASE_SCOPES, CHANNEL_HISTORY_SCOPE, scopes};

    #[test]
    fn default_login_is_least_privilege() {
        assert!(!BASE_SCOPES.contains(CHANNEL_HISTORY_SCOPE));
        assert_eq!(scopes(false), BASE_SCOPES);
    }

    #[test]
    fn channel_history_is_explicitly_opted_in() {
        assert!(scopes(true).contains(CHANNEL_HISTORY_SCOPE));
    }
}
