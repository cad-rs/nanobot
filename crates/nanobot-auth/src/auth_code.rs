//! OAuth 2.0 sign-in in the system browser: authorization code + PKCE
//! ([RFC 7636]), with the redirect coming back to a loopback port
//! ([RFC 8252] §7.3). Desktop only.
//!
//! The device flow ([`crate::oauth`]) asks the user to type a code; this one
//! asks for nothing — the product opens [`SignInRequest::url`] and the browser
//! comes back by itself. Both end in the same [`TokenSet`], so the rest of the
//! account surface is shared: hand it to
//! [`OAuth2Provider::adopt`](crate::OAuth2Provider::adopt), refresh it through
//! the same token endpoint, persist it with
//! [`persist_tokens`](crate::persist_tokens).
//!
//! The split of responsibilities is the device flow's too. [`AuthCodeFlow`] is
//! the transport: [`begin`](AuthCodeFlow::begin) is synchronous and
//! network-free, [`poll`](AuthCodeFlow::poll) is an `async fn` the caller's
//! executor drives, and the caller owns the polling loop and its timer (about
//! once a second is plenty). Every endpoint comes from [`AuthCodeConfig`], so
//! no discovery document is needed.
//!
//! What keeps the redirect honest: only the browser the request was sent to
//! knows the `state`, so a callback without it is turned away *without ending
//! the wait*; the code is useless without the verifier, which never leaves
//! this process; and the browser only ever gets fixed text back.
//!
//! Ported from workbench-rs's `workbench-core/src/auth_code`. Its web variant
//! (a same-origin callback page) is not ported.
//!
//! [RFC 7636]: https://www.rfc-editor.org/rfc/rfc7636
//! [RFC 8252]: https://www.rfc-editor.org/rfc/rfc8252

use std::io::{BufRead, BufReader, ErrorKind, Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest as _, Sha256};

use crate::base64;
use crate::error::AuthError;
use crate::now_ms;
use crate::oauth::{finish, token_set, TokenSet};

/// Where and how to sign in. Plain data the product fills in.
#[derive(Clone, Debug)]
pub struct AuthCodeConfig {
    pub authorization_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    /// Some providers require an audience parameter (Auth0-style).
    pub audience: Option<String>,
    /// The loopback port of the redirect; 0 takes any free one. Pin it for a
    /// backend that only accepts registered redirect URIs.
    pub redirect_port: u16,
    /// The path of the redirect on the loopback port.
    pub redirect_path: String,
    /// How long to wait for the user to finish in the browser.
    pub timeout_ms: u64,
}

impl AuthCodeConfig {
    pub fn new(
        authorization_url: impl Into<String>,
        token_url: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Self {
        Self {
            authorization_url: authorization_url.into(),
            token_url: token_url.into(),
            client_id: client_id.into(),
            scopes: vec!["openid".into(), "profile".into(), "email".into()],
            audience: None,
            redirect_port: 0,
            redirect_path: "/callback".to_string(),
            timeout_ms: 300_000,
        }
    }
}

/// What [`AuthCodeFlow::begin`] hands back: the page to open in the browser.
/// There is nothing for the user to type.
#[derive(Clone, Debug)]
pub struct SignInRequest {
    pub url: String,
    /// How long the flow waits for the browser to come back.
    pub expires_in_ms: u64,
}

/// The outcome of one [`AuthCodeFlow::poll`] round.
#[derive(Debug)]
pub enum AuthCodePoll {
    /// The browser has not come back yet; poll again in a moment.
    Pending,
    Authorized(TokenSet),
}

/// One sign-in until the browser comes back.
struct Login {
    listener: TcpListener,
    redirect_uri: String,
    state: String,
    verifier: String,
    deadline_ms: u64,
}

/// The authorization-code transport. All methods take `&self`.
pub struct AuthCodeFlow {
    config: AuthCodeConfig,
    http: reqwest::Client,
    pending: Mutex<Option<Login>>,
}

impl AuthCodeFlow {
    pub fn new(config: AuthCodeConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            pending: Mutex::new(None),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Option<Login>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts a sign-in: listens on the loopback port and returns the page to
    /// open. Starting again abandons the previous attempt.
    pub fn begin(&self) -> Result<SignInRequest, AuthError> {
        let config = &self.config;
        let mut pending = self.pending();
        // Let go of a fixed port an abandoned sign-in still holds.
        *pending = None;
        let listening = |e: std::io::Error| {
            AuthError::Config(format!("could not listen for the browser: {e}"))
        };
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.redirect_port))
            .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
            .map_err(listening)?;
        let port = listener.local_addr().map_err(listening)?.port();
        let login = Login {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}{}", config.redirect_path),
            state: random_token()?,
            verifier: random_token()?,
            deadline_ms: now_ms().saturating_add(config.timeout_ms),
        };

        let mut query = form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", &login.redirect_uri)
            .append_pair("state", &login.state)
            .append_pair("code_challenge", &code_challenge(&login.verifier))
            .append_pair("code_challenge_method", "S256");
        if !config.scopes.is_empty() {
            query.append_pair("scope", &config.scopes.join(" "));
        }
        if let Some(audience) = &config.audience {
            query.append_pair("audience", audience);
        }
        let separator = if config.authorization_url.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!("{}{separator}{}", config.authorization_url, query.finish());
        *pending = Some(login);
        Ok(SignInRequest {
            url,
            expires_in_ms: config.timeout_ms,
        })
    }

    /// Abandons a sign-in in progress and frees its port.
    pub fn cancel(&self) {
        *self.pending() = None;
    }

    /// One round: has the browser come back? [`AuthCodePoll::Authorized`] and
    /// every error end the flow; [`AuthCodePoll::Pending`] means poll again.
    pub async fn poll(&self) -> Result<AuthCodePoll, AuthError> {
        let Some((mut stream, login, code)) = self.take_code()? else {
            return Ok(AuthCodePoll::Pending);
        };
        let exchanged = self.exchange(&code, &login).await;
        let message = match &exchanged {
            Ok(_) => "Signed in. You can close this tab.",
            Err(_) => "Sign-in failed. Return to the app and try again.",
        };
        respond(&mut stream, "200 OK", message);
        exchanged.map(AuthCodePoll::Authorized)
    }

    /// The synchronous half of a poll: the browser's connection, the login it
    /// answers and its code — or `None` while there is nothing to exchange.
    fn take_code(&self) -> Result<Option<(TcpStream, Login, String)>, AuthError> {
        let mut pending = self.pending();
        let Some(login) = pending.as_ref() else {
            return Err(AuthError::Config("sign-in has not started".into()));
        };
        if now_ms() >= login.deadline_ms {
            *pending = None;
            return Err(AuthError::Expired);
        }
        let mut stream = match login.listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
            Err(e) => {
                return Err(AuthError::Protocol(format!(
                    "could not read the browser's reply: {e}"
                )))
            }
        };
        let Some(query) = read_callback_query(&mut stream, &self.config.redirect_path) else {
            respond(&mut stream, "404 Not Found", "Not found.");
            return Ok(None);
        };
        match read_callback(&query, &login.state) {
            Callback::Forged => {
                respond(&mut stream, "400 Bad Request", "This sign-in request is not valid.");
                Ok(None)
            }
            Callback::Over(error) => {
                *pending = None;
                respond(&mut stream, "200 OK", "Sign-in did not complete. You can close this tab.");
                Err(error)
            }
            Callback::Code(code) => {
                let login = pending.take().expect("checked above");
                Ok(Some((stream, login, code)))
            }
        }
    }

    /// Trades the authorization code for tokens.
    async fn exchange(&self, code: &str, login: &Login) -> Result<TokenSet, AuthError> {
        let response = self
            .http
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", login.redirect_uri.as_str()),
                ("client_id", self.config.client_id.as_str()),
                ("code_verifier", login.verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|_| AuthError::Protocol("token exchange request failed".into()))?;
        finish(response, token_set, "token exchange").await
    }
}

/// What the query of a redirect says about the sign-in with `state`.
#[derive(Debug, PartialEq)]
enum Callback {
    /// Not this sign-in's: only the browser it was sent to knows the state.
    Forged,
    /// The sign-in ended at the page.
    Over(AuthError),
    Code(String),
}

fn read_callback(query: &str, state: &str) -> Callback {
    let param = |name: &str| {
        form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if param("state").as_deref() != Some(state) {
        return Callback::Forged;
    }
    match (param("error"), param("code")) {
        (Some(error), _) if error == "access_denied" => {
            Callback::Over(AuthError::AuthorizationDenied)
        }
        (Some(error), _) => Callback::Over(AuthError::Protocol(format!(
            "the sign-in page returned an error: {error:.80}"
        ))),
        (None, Some(code)) => Callback::Code(code),
        (None, None) => Callback::Over(AuthError::Protocol(
            "the sign-in page returned no code".into(),
        )),
    }
}

/// The query string of a `GET` for `path`; None for any other request. The
/// whole header is read, because closing a socket with unread data can reset
/// the connection before the browser sees the response.
fn read_callback_query(stream: &mut TcpStream, path: &str) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut reader = BufReader::new(stream.take(16 * 1024));
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut line = String::new();
    while matches!(reader.read_line(&mut line), Ok(n) if n > 0) && line.trim_end() != "" {
        line.clear();
    }
    let target = request_line.strip_prefix("GET ")?.split_whitespace().next()?;
    let (target_path, query) = target.split_once('?')?;
    (target_path == path).then(|| query.to_string())
}

/// `message` is always fixed text: nothing from the request reaches the page.
fn respond(stream: &mut TcpStream, status: &str, message: &str) {
    let body = format!("<!doctype html><meta charset=utf-8><title>Sign-in</title><p>{message}</p>");
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
}

/// 256 random bits, URL-safe: long enough for a PKCE verifier (43 characters).
fn random_token() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| AuthError::Config(format!("no secure randomness available: {e}")))?;
    Ok(base64::encode_urlsafe(&bytes))
}

fn code_challenge(verifier: &str) -> String {
    base64::encode_urlsafe(&Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redirect_is_read_only_if_it_carries_this_sign_ins_state() {
        assert_eq!(read_callback("code=c&state=other", "s"), Callback::Forged);
        assert_eq!(read_callback("code=c", "s"), Callback::Forged);
        assert_eq!(
            read_callback("code=a%2Fb&state=s", "s"),
            Callback::Code("a/b".to_string())
        );
        assert_eq!(
            read_callback("error=access_denied&state=s", "s"),
            Callback::Over(AuthError::AuthorizationDenied)
        );
        assert!(matches!(
            read_callback("error=server_error&state=s", "s"),
            Callback::Over(AuthError::Protocol(detail)) if detail.contains("server_error")
        ));
        assert!(matches!(
            read_callback("state=s", "s"),
            Callback::Over(AuthError::Protocol(_))
        ));
    }

    /// RFC 7636 Appendix B.
    #[test]
    fn the_challenge_matches_the_rfc_example() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn random_tokens_are_long_enough_for_pkce_and_differ() {
        let first = random_token().unwrap();
        assert!((43..=128).contains(&first.len()));
        assert_ne!(first, random_token().unwrap());
    }
}
