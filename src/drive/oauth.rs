use super::Failure;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[derive(Clone)]
pub struct Client {
    pub id: String,
    pub secret: String,
}
impl Client {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 64 * 1024 {
            return Err("The OAuth configuration file is too large.".into());
        }
        #[derive(Deserialize)]
        struct Document {
            installed: Option<Installed>,
        }
        #[derive(Deserialize)]
        struct Installed {
            client_id: String,
            #[serde(default)]
            client_secret: String,
        }
        let file: Document = serde_json::from_slice(bytes).map_err(
            |_| "Choose a Google OAuth client JSON downloaded from Google Cloud Console.",
        )?;
        let installed = file.installed.ok_or("This is not a Desktop app OAuth client. Create a client of type Desktop app in Google Cloud Console.")?;
        if installed.client_id.len() > 512
            || !installed.client_id.ends_with(".apps.googleusercontent.com")
            || installed
                .client_id
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && !b"-._".contains(&b))
        {
            return Err("The OAuth client ID is invalid.".into());
        }
        if installed.client_secret.len() > 4096 || installed.client_secret.contains(['\r', '\n']) {
            return Err("The OAuth client configuration is invalid.".into());
        }
        // Authorization and token URLs from imported files are deliberately ignored.
        Ok(Self {
            id: installed.client_id,
            secret: installed.client_secret,
        })
    }
}

#[derive(Clone)]
pub struct Token {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_in: u64,
}
pub fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .build()
        .into()
}

fn random_urlsafe() -> Result<String, Failure> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| Failure::data("Windows could not generate secure sign-in randomness."))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
pub fn authorization_url(client: &Client, redirect: &str, state: &str, verifier: &str) -> String {
    let mut url = url::Url::parse(AUTH_URL).unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", client.id.as_str()),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", &challenge(verifier)),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent select_account"),
    ]);
    url.into()
}

enum Callback {
    Code(String),
    Denied,
}
fn parse_callback(target: &str, expected_state: &str) -> Result<Callback, &'static str> {
    if !target.starts_with("/?") || target.len() > 8192 {
        return Err("Unexpected callback path");
    }
    let url =
        url::Url::parse(&format!("http://127.0.0.1{target}")).map_err(|_| "Invalid callback")?;
    let values: Vec<_> = url.query_pairs().collect();
    let single = |name: &str| -> Result<Option<String>, &'static str> {
        let matches: Vec<_> = values.iter().filter(|(key, _)| key == name).collect();
        if matches.len() > 1 {
            return Err("Duplicate authorization parameter");
        }
        Ok(matches.first().map(|(_, value)| value.to_string()))
    };
    if single("state")?.as_deref() != Some(expected_state) {
        return Err("Invalid authorization state");
    }
    if single("error")?.is_some() {
        return Ok(Callback::Denied);
    }
    let code = single("code")?
        .filter(|code| !code.is_empty() && code.len() <= 4096)
        .ok_or("Missing authorization code")?;
    Ok(Callback::Code(code))
}

fn callback_request(stream: &mut TcpStream, port: u16, state: &str) -> Result<Callback, ()> {
    stream.set_nonblocking(false).map_err(|_| ())?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let deadline = Instant::now() + Duration::from_secs(2);
    while bytes.len() < 12 * 1024 {
        if Instant::now() >= deadline {
            return Err(());
        }
        let _ = stream.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())));
        let count = stream.read(&mut buffer).map_err(|_| ())?;
        if count == 0 {
            return Err(());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    if bytes.len() > 12 * 1024 || !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
        return Err(());
    }
    let request = std::str::from_utf8(&bytes).map_err(|_| ())?;
    let mut lines = request.split("\r\n");
    let first: Vec<_> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    let host = lines.find_map(|line| {
        line.split_once(':')
            .filter(|(key, _)| key.eq_ignore_ascii_case("host"))
            .map(|(_, value)| value.trim())
    });
    let valid_host = host == Some(format!("127.0.0.1:{port}").as_str());
    let parsed = if first.len() == 3 && first[0] == "GET" && valid_host {
        parse_callback(first[1], state).map_err(|_| ())
    } else {
        Err(())
    };
    let (status, text) = if parsed.is_ok() {
        (
            "200 OK",
            "Google authorization received. Return to Subscriptions Reminder to check the connection. You can close this tab.",
        )
    } else {
        (
            "400 Bad Request",
            "This authorization response was not accepted. Return to the application or finish the Google sign-in in the original browser tab.",
        )
    };
    let body = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>Subscriptions Reminder</title><body><h1>Subscriptions Reminder</h1><p>{text}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    parsed
}

pub fn sign_in(
    client: &Client,
    cancel: &AtomicBool,
    stopped: &AtomicBool,
    ready: impl FnOnce(String),
) -> Result<Token, Failure> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|_| Failure::data("Cannot open the local Google sign-in callback. Check whether another application or firewall blocks loopback connections."))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| Failure::data("Cannot initialize Google sign-in."))?;
    let port = listener
        .local_addr()
        .map_err(|_| Failure::data("Cannot read the callback address."))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}/");
    let state = random_urlsafe()?;
    let verifier = random_urlsafe()?;
    let url = authorization_url(client, &redirect, &state, &verifier);
    ready(url.clone());
    webbrowser::open(&url).map_err(|_| {
        Failure::data(
            "Could not open the system browser. Use Open browser again or try reconnecting.",
        )
    })?;
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        if cancel.load(Ordering::SeqCst) || stopped.load(Ordering::SeqCst) {
            return Err(Failure::data("Google sign-in was cancelled."));
        }
        match listener.accept() {
            Ok((mut stream, _)) => match callback_request(&mut stream, port, &state) {
                Ok(Callback::Code(code)) => {
                    return exchange_at(TOKEN_URL, client, &code, &redirect, &verifier);
                }
                Ok(Callback::Denied) => {
                    return Err(Failure::data(
                        "Google sign-in or Drive permission was declined. Your local data was not changed.",
                    ));
                }
                Err(()) => (),
            },
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(_) => {
                return Err(Failure::data(
                    "The local Google sign-in callback stopped working. Try connecting again.",
                ));
            }
        }
    }
    Err(Failure::data(
        "Google sign-in timed out after three minutes. Click Connect Google Drive to try again.",
    ))
}

fn parse_token(mut response: ureq::http::Response<ureq::Body>) -> Result<Token, Failure> {
    let status = response.status().as_u16();
    let value: serde_json::Value = response
        .body_mut()
        .with_config()
        .limit(128 * 1024)
        .read_json()
        .map_err(|_| {
            Failure::retry("Google returned an invalid token response. Try connecting again.")
        })?;
    if !response.status().is_success() {
        return Err(match value.get("error").and_then(|v| v.as_str()) {
            Some("invalid_grant") => Failure::sign_in(
                "The Google session expired or was revoked. Reconnect Google Drive; local changes are preserved.",
            ),
            Some("invalid_client" | "unauthorized_client") => Failure::sign_in(
                "Google rejected the OAuth client. Import a Desktop app client JSON from the correct Google Cloud project.",
            ),
            _ if status == 429 || status >= 500 => {
                Failure::retry("Google sign-in is temporarily unavailable. Try again later.")
            }
            _ => Failure::sign_in(
                "Google could not authorize this application. Check the OAuth client, consent screen, test users, and Drive permission.",
            ),
        });
    }
    if value
        .get("token_type")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.eq_ignore_ascii_case("bearer"))
    {
        return Err(Failure::data("Google returned an unsupported token type."));
    }
    if let Some(scope) = value.get("scope").and_then(|v| v.as_str())
        && !scope.split_whitespace().any(|s| s == SCOPE)
    {
        return Err(Failure::sign_in(
            "Google Drive application-data permission was not granted. Connect again and allow that permission.",
        ));
    }
    let access = value
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && s.len() <= 16 * 1024)
        .ok_or_else(|| Failure::data("Google returned no usable access token."))?
        .to_string();
    let refresh = value
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && s.len() <= 16 * 1024)
        .map(str::to_string);
    let expires_in = value
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600)
        .clamp(60, 86_400);
    Ok(Token {
        access,
        refresh,
        expires_in,
    })
}
fn exchange_at(
    endpoint: &str,
    client: &Client,
    code: &str,
    redirect: &str,
    verifier: &str,
) -> Result<Token, Failure> {
    let mut fields = vec![
        ("client_id", client.id.as_str()),
        ("code", code),
        ("redirect_uri", redirect),
        ("code_verifier", verifier),
        ("grant_type", "authorization_code"),
    ];
    if !client.secret.is_empty() {
        fields.push(("client_secret", &client.secret));
    }
    let response = http().post(endpoint).send_form(fields).map_err(|_| {
        Failure::retry(
            "Cannot contact Google to finish sign-in. Check the connection and try again.",
        )
    })?;
    parse_token(response)
}
pub fn refresh(client: &Client, refresh: &str) -> Result<Token, Failure> {
    refresh_at(TOKEN_URL, client, refresh)
}
fn refresh_at(endpoint: &str, client: &Client, refresh: &str) -> Result<Token, Failure> {
    let mut fields = vec![
        ("client_id", client.id.as_str()),
        ("refresh_token", refresh),
        ("grant_type", "refresh_token"),
    ];
    if !client.secret.is_empty() {
        fields.push(("client_secret", &client.secret));
    }
    parse_token(http().post(endpoint).send_form(fields).map_err(|_| {
        Failure::retry(
            "Cannot refresh the Google session. Working offline; local changes will be retried.",
        )
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::test_support::server;
    use serde_json::json;
    #[test]
    fn pkce_matches_rfc_vector_and_authorization_requests_minimal_offline_scope() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let url = url::Url::parse(&authorization_url(
            &Client {
                id: "sample.apps.googleusercontent.com".into(),
                secret: "never-in-url".into(),
            },
            "http://127.0.0.1:1234/",
            "state",
            "verifier",
        ))
        .unwrap();
        let values: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(values["scope"], SCOPE);
        assert_eq!(values["code_challenge_method"], "S256");
        assert_eq!(values["access_type"], "offline");
        assert!(!url.as_str().contains("never-in-url"));
    }
    #[test]
    fn callbacks_reject_wrong_state_duplicates_and_noncallback_paths() {
        assert!(
            matches!(parse_callback("/?state=right&code=sample%2Bcode", "right"), Ok(Callback::Code(code)) if code=="sample+code")
        );
        assert!(parse_callback("/?state=wrong&code=sample", "right").is_err());
        assert!(parse_callback("/?state=right&state=right&code=sample", "right").is_err());
        assert!(parse_callback("/favicon.ico", "right").is_err());
        assert!(matches!(
            parse_callback("/?state=right&error=access_denied", "right"),
            Ok(Callback::Denied)
        ));
    }
    #[test]
    fn callback_accepts_split_http_headers_on_a_nonblocking_windows_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let sender = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(stream, "GET /?state=right&code=test-code HTTP/1.1\r\n").unwrap();
            std::thread::sleep(Duration::from_millis(30));
            write!(stream, "Host: 127.0.0.1:{port}\r\n\r\n").unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            assert!(reply.starts_with("HTTP/1.1 200"));
            assert!(!reply.contains("test-code"));
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        assert!(
            matches!(callback_request(&mut stream,port,"right"),Ok(Callback::Code(code)) if code=="test-code")
        );
        drop(stream);
        sender.join().unwrap();
    }
    #[test]
    fn only_desktop_credentials_are_imported_and_foreign_endpoints_are_ignored() {
        assert!(
            Client::parse(br#"{"web":{"client_id":"sample.apps.googleusercontent.com"}}"#).is_err()
        );
        let client = Client::parse(br#"{"installed":{"client_id":"sample.apps.googleusercontent.com","client_secret":"sample","token_uri":"https://attacker.invalid"}}"#).unwrap();
        assert_eq!(client.id, "sample.apps.googleusercontent.com");
        assert!(
            !authorization_url(&client, "http://127.0.0.1:1234/", "state", "verifier")
                .contains("attacker")
        );
    }
    #[test]
    fn token_exchange_sends_pkce_and_refresh_supports_rotation() {
        let (root, worker) = server(vec![
            (
                200,
                json!({"access_token":"access1","refresh_token":"refresh1","expires_in":3600,"token_type":"Bearer","scope":SCOPE}),
            ),
            (
                200,
                json!({"access_token":"access2","refresh_token":"refresh2","expires_in":3600}),
            ),
        ]);
        let client = Client {
            id: "sample.apps.googleusercontent.com".into(),
            secret: "test-secret".into(),
        };
        let first = exchange_at(
            &format!("{root}/token"),
            &client,
            "test-code",
            "http://127.0.0.1:1234/",
            "test-verifier",
        )
        .unwrap();
        assert_eq!(first.refresh.as_deref(), Some("refresh1"));
        let next = refresh_at(&format!("{root}/token"), &client, "refresh1").unwrap();
        assert_eq!(next.refresh.as_deref(), Some("refresh2"));
        let requests = worker.join().unwrap();
        let form: std::collections::HashMap<_, _> = url::form_urlencoded::parse(&requests[0].body)
            .into_owned()
            .collect();
        assert_eq!(form["code_verifier"], "test-verifier");
        assert_eq!(form["client_secret"], "test-secret");
        assert_eq!(form["redirect_uri"], "http://127.0.0.1:1234/");
        assert!(!requests[0].target.contains("test-secret"));
        let form: std::collections::HashMap<_, _> = url::form_urlencoded::parse(&requests[1].body)
            .into_owned()
            .collect();
        assert_eq!(form["grant_type"], "refresh_token");
        assert_eq!(form["refresh_token"], "refresh1");
    }
    #[test]
    fn revoked_sessions_and_missing_permissions_are_reported_without_echoing_secrets() {
        let (root, worker) = server(vec![
            (
                400,
                json!({"error":"invalid_grant","error_description":"private-data-never-display"}),
            ),
            (200, json!({"access_token":"access","scope":"openid"})),
        ]);
        let client = Client {
            id: "sample.apps.googleusercontent.com".into(),
            secret: String::new(),
        };
        let error = refresh_at(&format!("{root}/token"), &client, "test-refresh")
            .err()
            .unwrap();
        assert!(error.sign_in);
        assert!(!error.message.contains("private-data-never-display"));
        assert!(
            refresh_at(&format!("{root}/token"), &client, "test-refresh")
                .err()
                .unwrap()
                .sign_in
        );
        worker.join().unwrap();
    }
}
