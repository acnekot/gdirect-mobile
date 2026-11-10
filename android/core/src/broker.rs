//! Signing in through the person's own token server.
//!
//! Where Google's sign-in pages cannot be reached, a small server elsewhere
//! (see `server/` in this repository) keeps the refresh token and hands out
//! short-lived access tokens. Only tokens pass through it: Drive traffic still
//! goes from this device straight to Google.
use crate::store::Store;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Broker {
    /// Base address of the server, e.g. `https://drive.example.com`.
    pub url: String,
    /// The shared secret the server was started with.
    pub key: String,
}
#[derive(Debug)]
pub struct Reply {
    pub access_token: String,
    pub expires_in: i64,
    pub email: Option<String>,
}

fn setting(account: &str) -> String {
    format!("mobile_broker:{account}")
}
/// Credentials stored for a server account so the account table stays uniform.
pub const PLACEHOLDER: &str =
    r#"{"type":"authorized_user","client_id":"token-server","client_secret":"","refresh_token":"token-server"}"#;
pub const PLACEHOLDER_CLIENT: &str = "token-server";

pub fn saved(db: &Store, account: &str) -> Result<Option<Broker>> {
    Ok(match db.setting(&setting(account))? {
        Some(raw) => Some(serde_json::from_str(&raw).context("stored token server is unreadable")?),
        None => None,
    })
}
pub fn save(db: &Store, account: &str, broker: &Broker) -> Result<()> {
    db.set(&setting(account), &serde_json::to_string(broker)?)
}
pub fn forget(db: &Store, account: &str) -> Result<()> {
    db.unset(&setting(account))
}

/// Normalises what a person types: scheme required, no trailing slash.
pub fn normalise(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    let parsed = url::Url::parse(url).context("the server address is not a URL")?;
    ensure!(
        matches!(parsed.scheme(), "https" | "http") && parsed.host_str().is_some(),
        "the server address must start with https:// or http://"
    );
    Ok(url.to_owned())
}

pub fn fetch(broker: &Broker) -> Result<Reply> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(40))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .post(format!("{}/token", broker.url))
        .bearer_auth(&broker.key)
        .send()
        .map_err(|e| anyhow::anyhow!("cannot reach the token server ({})", e.without_url()))?;
    let status = response.status();
    let data: serde_json::Value = response.json().unwrap_or_default();
    if !status.is_success() {
        let why = data["error"].as_str().unwrap_or("no details");
        if status.as_u16() == 401 || status.as_u16() == 403 {
            bail!("the token server refused the access key ({status}: {why})");
        }
        bail!("the token server answered {status}: {why}");
    }
    let access_token = data["access_token"]
        .as_str()
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_graphic()))
        .context("the token server returned no access token")?;
    Ok(Reply {
        access_token: access_token.to_owned(),
        expires_in: data["expires_in"].as_i64().unwrap_or(3000).clamp(60, 3600),
        email: data["email"].as_str().map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn serve(reply: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let mut stream = stream.unwrap();
                let mut buffer = [0u8; 2048];
                let n = stream.read(&mut buffer).unwrap();
                let request = String::from_utf8_lossy(&buffer[..n]).to_lowercase();
                let ok = request.starts_with("post /token") && request.contains("authorization: bearer s3cret");
                let (status, body) = if ok { ("200 OK", reply) } else { ("401 Unauthorized", r#"{"error":"bad key"}"#) };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    fn tokens_come_from_the_server() {
        let url = serve(r#"{"access_token":"ya29.abc","expires_in":3599,"email":"a@b.c"}"#);
        let reply = fetch(&Broker { url, key: "s3cret".into() }).unwrap();
        assert_eq!(reply.access_token, "ya29.abc");
        assert_eq!(reply.expires_in, 3599);
        assert_eq!(reply.email.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn a_wrong_key_is_reported() {
        let url = serve("{}");
        let error = fetch(&Broker { url, key: "nope".into() }).unwrap_err();
        assert!(format!("{error}").contains("refused the access key"), "{error}");
    }

    #[test]
    fn addresses_are_normalised() {
        assert_eq!(normalise(" https://x.example/ ").unwrap(), "https://x.example");
        assert!(normalise("x.example").is_err());
        assert!(normalise("ftp://x.example").is_err());
    }
}
