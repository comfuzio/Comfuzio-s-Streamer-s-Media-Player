//! OAuth 2.0 helpers shared by the Spotify, Google (YouTube) and Twitch clients:
//! PKCE (RFC 7636), loopback redirect capture (RFC 8252) and token persistence.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::error::{AppError, AppResult};
use crate::util::now_ms;

/// Tokens as persisted in the kv store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ms: i64,
    #[serde(default)]
    pub scope: String,
}

impl StoredTokens {
    pub fn new(access_token: String, refresh_token: String, expires_in_secs: i64, scope: String) -> Self {
        Self { access_token, refresh_token, expires_at_ms: now_ms() + expires_in_secs * 1000, scope }
    }

    /// True if the access token expires within `margin_ms`.
    pub fn expires_within(&self, margin_ms: i64) -> bool {
        self.expires_at_ms - now_ms() <= margin_ms
    }
}

pub fn random_urlsafe(bytes: usize) -> AppResult<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| AppError::Internal(format!("secure random unavailable: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Accepts loopback connections until the browser hits `/callback`, answering
/// each request with a small page. Returns the authorization code.
pub async fn accept_callback(listener: &TcpListener, expected_state: &str, service: &str) -> AppResult<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let mut len = 0;
        while len < buf.len() {
            let n = stream.read(&mut buf[len..]).await?;
            if n == 0 {
                break;
            }
            len += n;
            if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf[..len]);
        let target = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/");
        let parsed = Url::parse(&format!("http://127.0.0.1{target}")).ok();

        let outcome: Option<Result<String, String>> = parsed.as_ref().filter(|u| u.path() == "/callback").map(|u| {
            let get = |k: &str| u.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned());
            if let Some(err) = get("error") {
                Err(format!("authorization denied: {err}"))
            } else if get("state").as_deref() != Some(expected_state) {
                Err("state mismatch (possible CSRF); please retry the login".into())
            } else {
                get("code").ok_or_else(|| "callback had no code".into())
            }
        });

        let (status, message) = match &outcome {
            Some(Ok(_)) => ("200 OK", format!("{service} connected. You can close this tab and return to the player.")),
            Some(Err(_)) => ("400 Bad Request", format!("{service} login failed. Return to the player for details.")),
            None => ("404 Not Found", "Not found.".to_owned()),
        };
        let body = format!(
            "<!doctype html><meta charset=utf-8><title>Comfuzio Player</title>\
             <body style=\"font-family:system-ui;padding:3rem;text-align:center\"><h2>{message}</h2></body>"
        );
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;

        match outcome {
            Some(Ok(code)) => return Ok(code),
            Some(Err(msg)) => return Err(AppError::Invalid(format!("{service}: {msg}"))),
            None => continue, // favicon etc.
        }
    }
}

/// Truncates `body` for inclusion in error messages.
pub fn excerpt(body: &str) -> String {
    body.chars().take(240).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
