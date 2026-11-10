//! Integration tests for the browser sign-in: a fake token endpoint on a
//! local TCP socket and raw sockets standing in for the browser. No IdP, no
//! real browser.
#![cfg(not(target_family = "wasm"))]

use nanobot_auth::{AuthCodeConfig, AuthCodeFlow, AuthCodePoll, AuthError, TokenSet};
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;

const TOKEN_REPLY: &str = r#"{
    "access_token": "at-1",
    "refresh_token": "rt-1",
    "expires_in": 3600,
    "token_type": "Bearer"
}"#;

/// A token endpoint that answers each request with the next reply and
/// records the request it got.
fn serve(replies: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, reply) in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // TCP may split headers and body across segments; read until the
            // request is complete (headers + content-length bytes).
            let mut buf = Vec::new();
            let mut chunk = [0_u8; 8192];
            loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().ok())
                        })
                        .flatten()
                        .unwrap_or(0);
                    if buf.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            let reason = if status == 200 { "OK" } else { "Bad Request" };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nConnection: close\r\n\
                     Content-Length: {}\r\n\r\n{reply}",
                    reply.len()
                )
                .as_bytes(),
            );
            let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
        }
    });
    (format!("http://{addr}"), rx)
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn params(query: &str) -> HashMap<String, String> {
    form_urlencoded::parse(query.as_bytes()).into_owned().collect()
}

fn config(token_base: &str) -> AuthCodeConfig {
    AuthCodeConfig::new(
        "https://idp.test/authorize?tenant=acme",
        format!("{token_base}/oauth/token"),
        "client-1",
    )
}

/// A started flow and what the sign-in page was told.
fn started(config: AuthCodeConfig) -> (AuthCodeFlow, HashMap<String, String>) {
    let flow = AuthCodeFlow::new(config);
    let request = flow.begin().expect("begin");
    let (page, query) = request.url.split_once("?tenant=acme&").expect("the query is appended");
    assert_eq!(page, "https://idp.test/authorize");
    (flow, params(query))
}

/// The browser following a redirect to `redirect_uri` with `query`; returns
/// what the page showed.
fn browser(redirect_uri: &str, query: &str) -> std::thread::JoinHandle<String> {
    let rest = redirect_uri.strip_prefix("http://").expect("a loopback redirect");
    let (host, path) = rest.split_once('/').expect("a path");
    let (host, request) = (
        host.to_string(),
        format!("GET /{path}?{query} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    std::thread::spawn(move || {
        let mut stream = TcpStream::connect(&host).expect("the flow is listening");
        stream.write_all(request.as_bytes()).unwrap();
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply);
        reply
    })
}

/// Polls until the flow stops answering `Pending`.
fn finish(flow: &AuthCodeFlow) -> Result<TokenSet, AuthError> {
    block_on(async {
        for _ in 0..500 {
            match flow.poll().await? {
                AuthCodePoll::Authorized(tokens) => return Ok(tokens),
                AuthCodePoll::Pending => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
        panic!("the sign-in never finished");
    })
}

#[test]
fn the_page_is_asked_for_a_code_with_pkce_and_a_loopback_redirect() {
    let (_flow, sent) = started(config("http://127.0.0.1:9"));
    assert_eq!(sent["response_type"], "code");
    assert_eq!(sent["client_id"], "client-1");
    assert_eq!(sent["scope"], "openid profile email");
    assert_eq!(sent["code_challenge_method"], "S256");
    assert!(sent["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert!(sent["redirect_uri"].ends_with("/callback"));
    assert!(sent["state"].len() >= 43);
}

#[test]
fn a_code_is_exchanged_with_its_verifier_and_ends_in_tokens() {
    let (base, requests) = serve(vec![(200, TOKEN_REPLY)]);
    let (flow, sent) = started(config(&base));
    assert!(matches!(block_on(flow.poll()), Ok(AuthCodePoll::Pending)));

    let page = browser(
        &sent["redirect_uri"],
        &format!("code=abc%2F1&state={}", sent["state"]),
    );
    let tokens = finish(&flow).expect("signed in");
    assert_eq!(tokens.access_token, "at-1");
    assert_eq!(tokens.refresh_token.as_deref(), Some("rt-1"));
    assert!(tokens.expires_at_ms.is_some());

    let reply = page.join().unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert!(reply.contains("Signed in."));
    assert!(!reply.contains("abc"), "nothing from the request reaches the page");

    let request = requests.recv().unwrap();
    let form = params(request.split("\r\n\r\n").nth(1).unwrap());
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "abc/1");
    assert_eq!(form["client_id"], "client-1");
    assert_eq!(form["redirect_uri"], sent["redirect_uri"]);
    // PKCE: the verifier sent to the token endpoint is the one whose hash
    // the sign-in page was given.
    let digest = Sha256::digest(form["code_verifier"].as_bytes());
    assert_eq!(base64_url(&digest), sent["code_challenge"]);

    assert!(
        matches!(block_on(flow.poll()), Err(AuthError::Config(_))),
        "the sign-in is over"
    );
}

#[test]
fn a_callback_with_the_wrong_state_is_turned_away_without_ending_the_wait() {
    let (base, _requests) = serve(vec![(200, TOKEN_REPLY)]);
    let (flow, sent) = started(config(&base));

    let forged = browser(&sent["redirect_uri"], "code=stolen&state=forged");
    let reply = loop {
        assert!(matches!(block_on(flow.poll()), Ok(AuthCodePoll::Pending)));
        if forged.is_finished() {
            break forged.join().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");

    let stray = browser(&sent["redirect_uri"].replace("/callback", "/favicon.ico"), "x=1");
    let reply = loop {
        assert!(matches!(block_on(flow.poll()), Ok(AuthCodePoll::Pending)));
        if stray.is_finished() {
            break stray.join().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(reply.starts_with("HTTP/1.1 404"), "{reply}");

    let real = browser(
        &sent["redirect_uri"],
        &format!("code=real&state={}", sent["state"]),
    );
    assert_eq!(finish(&flow).expect("the real one still lands").access_token, "at-1");
    real.join().unwrap();
}

#[test]
fn the_user_refusing_is_a_denial() {
    let (flow, sent) = started(config("http://127.0.0.1:9"));
    let page = browser(
        &sent["redirect_uri"],
        &format!("error=access_denied&state={}", sent["state"]),
    );
    assert_eq!(finish(&flow).unwrap_err(), AuthError::AuthorizationDenied);
    assert!(page.join().unwrap().contains("did not complete"));
}

#[test]
fn a_refused_code_is_an_error_and_the_page_says_so() {
    let (base, _requests) = serve(vec![(400, r#"{"error": "invalid_grant"}"#)]);
    let (flow, sent) = started(config(&base));
    let page = browser(
        &sent["redirect_uri"],
        &format!("code=abc&state={}", sent["state"]),
    );
    assert!(matches!(
        finish(&flow),
        Err(AuthError::Http { status: 400, .. })
    ));
    assert!(page.join().unwrap().contains("Sign-in failed."));
}

#[test]
fn waiting_too_long_expires_and_polling_before_starting_fails() {
    let flow = AuthCodeFlow::new(config("http://127.0.0.1:9"));
    assert!(matches!(block_on(flow.poll()), Err(AuthError::Config(_))));

    let mut impatient = config("http://127.0.0.1:9");
    impatient.timeout_ms = 0;
    let flow = AuthCodeFlow::new(impatient);
    flow.begin().unwrap();
    assert_eq!(block_on(flow.poll()).unwrap_err(), AuthError::Expired);
}

#[test]
fn starting_again_frees_a_pinned_port_and_forgets_the_last_attempt() {
    // Find a free port, then pin it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (base, _requests) = serve(vec![(200, TOKEN_REPLY)]);
    let mut pinned = config(&base);
    pinned.redirect_port = port;
    pinned.redirect_path = "/signed-in".to_string();
    let flow = AuthCodeFlow::new(pinned);

    let first = flow.begin().expect("first attempt");
    let second = flow.begin().expect("the port was released for the second");
    let (first, second) = (
        params(first.url.split_once('&').unwrap().1),
        params(second.url.split_once('&').unwrap().1),
    );
    assert_eq!(
        second["redirect_uri"],
        format!("http://127.0.0.1:{port}/signed-in")
    );
    assert_ne!(first["state"], second["state"]);

    let stale = browser(
        &second["redirect_uri"],
        &format!("code=old&state={}", first["state"]),
    );
    while !stale.is_finished() {
        assert!(matches!(block_on(flow.poll()), Ok(AuthCodePoll::Pending)));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(stale.join().unwrap().starts_with("HTTP/1.1 400"));

    flow.cancel();
    assert!(matches!(block_on(flow.poll()), Err(AuthError::Config(_))));
    std::net::TcpListener::bind(("127.0.0.1", port)).expect("cancelling frees the port");
}

/// Unpadded base64url, as PKCE spells the challenge.
fn base64_url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let group = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(TABLE[(group >> (18 - 6 * i)) as usize & 0x3f] as char);
        }
    }
    out
}
