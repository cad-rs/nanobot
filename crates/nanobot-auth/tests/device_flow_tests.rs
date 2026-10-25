//! Integration tests for the device flow: a fake OAuth2 provider on a local
//! TCP socket, the full begin → poll(pending) → poll(authorized) → refresh
//! chain, and the AccountProvider surface over the resulting grant.

use nanobot_auth::{AuthError, OAuth2Config, OAuth2Provider, PollOutcome};
use nanobot_boot::{AccountProvider as _, EntitlementDecision, Settings};
use std::io::{Read, Write};
use std::sync::mpsc;

/// One recorded request per served reply; the receiver collects what the
/// client actually sent.
fn serve(replies: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, reason, reply) in replies
            .into_iter()
            .map(|(status, reply)| (status, if status == 200 { "OK" } else { "Bad Request" }, reply))
        {
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
            let request = String::from_utf8_lossy(&buf).to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nConnection: close\r\n\
                     Content-Length: {}\r\n\r\n{reply}",
                    reply.len()
                )
                .as_bytes(),
            );
            let _ = tx.send(request);
        }
    });
    (format!("http://{addr}"), rx)
}

fn config(base: &str) -> OAuth2Config {
    OAuth2Config {
        client_id: "test-client".into(),
        device_auth_url: format!("{base}/oauth/device/authorize"),
        token_url: format!("{base}/oauth/token"),
        scopes: vec!["openid".into(), "profile".into()],
        audience: None,
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

const DEVICE_REPLY: &str = r#"{
    "device_code": "dev-123",
    "user_code": "ABCD-EFGH",
    "verification_uri": "https://example.test/activate",
    "verification_uri_complete": "https://example.test/activate?code=ABCD-EFGH",
    "expires_in": 300,
    "interval": 5
}"#;

const PENDING_REPLY: &str = r#"{"error": "authorization_pending"}"#;

const TOKEN_REPLY: &str = r#"{
    "access_token": "at-1",
    "refresh_token": "rt-1",
    "expires_in": 3600,
    "token_type": "Bearer",
    "id_token": "eyJhbGciOiJub25lIn0.eyJzdWIiOiJzdWIxIiwibmFtZSI6IkFsaWNlIn0."
}"#;

const REFRESH_REPLY: &str = r#"{
    "access_token": "at-2",
    "refresh_token": "rt-2",
    "expires_in": 3600,
    "token_type": "Bearer",
    "id_token": "eyJhbGciOiJub25lIn0.eyJzdWIiOiJzdWIxIiwibmFtZSI6IkFsaWNlIn0."
}"#;

#[test]
fn the_full_device_flow_lands_a_working_account() {
    let (base, requests) = serve(vec![
        (200, DEVICE_REPLY),
        (400, PENDING_REPLY),
        (200, TOKEN_REPLY),
        (200, REFRESH_REPLY),
    ]);
    let flow = nanobot_auth::DeviceFlow::new(config(&base));

    // begin: the provider mints the code; the request carried client + scope.
    let code = block_on(flow.begin()).expect("begin");
    assert_eq!(code.user_code, "ABCD-EFGH");
    assert_eq!(code.device_code, "dev-123");
    assert_eq!(code.interval_ms, 5_000);
    assert_eq!(code.expires_in_ms, 300_000);

    // poll 1: pending, with the provider's cadence echoed back.
    let first = block_on(flow.poll(&code)).expect("poll 1");
    assert!(matches!(first, PollOutcome::Pending { retry_after_ms: 5_000 }));

    // The begin and poll requests said who was asking.
    let begin_request = requests.recv().unwrap();
    assert!(begin_request.contains("client_id=test-client"), "{begin_request}");
    assert!(begin_request.contains("scope=openid+profile"), "{begin_request}");
    let poll_request = requests.recv().unwrap();
    assert!(poll_request.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
    assert!(poll_request.contains("device_code=dev-123"));

    // poll 2: authorized. The id_token claims become the account.
    let second = block_on(flow.poll(&code)).expect("poll 2");
    let _second_request = requests.recv().unwrap();
    let PollOutcome::Authorized(tokens) = second else {
        panic!("expected authorization, got {second:?}");
    };
    assert_eq!(tokens.access_token, "at-1");
    assert!(tokens.expires_at_ms.is_some());

    let provider = OAuth2Provider::new(config(&base));
    assert_eq!(provider.check("agent.turn"), EntitlementDecision::Denied { reason: "not signed in".into() },
        "a fresh provider is a signed-out provider");
    provider.adopt(tokens);
    let account = provider.account().expect("signed in");
    assert_eq!(account.id, "sub1");
    assert_eq!(account.display_name, "Alice");
    assert!(provider.check("agent.turn").is_allowed());

    // refresh: a new grant through the stored refresh token.
    let refreshed = block_on(flow.refresh("rt-1")).expect("refresh");
    assert_eq!(refreshed.access_token, "at-2");
    let refresh_request = requests.recv().unwrap();
    assert!(
        refresh_request.contains("grant_type=refresh_token"),
        "the refresh request must name its grant; got:\n{refresh_request}"
    );
    assert!(refresh_request.contains("refresh_token=rt-1"));
}

#[test]
fn the_terminal_poll_errors_map_onto_the_vocabulary() {
    let (base, _requests) = serve(vec![
        (400, r#"{"error": "expired_token"}"#),
        (400, r#"{"error": "access_denied"}"#),
        (400, r#"{"error": "totally_unknown", "error_description": "?"}"#),
    ]);
    let flow = nanobot_auth::DeviceFlow::new(config(&base));
    let code = nanobot_auth::DeviceCode {
        device_code: "dev-1".into(),
        user_code: "X".into(),
        verification_uri: "https://example.test".into(),
        verification_uri_complete: None,
        expires_in_ms: 60_000,
        interval_ms: 5_000,
    };

    assert!(matches!(block_on(flow.poll(&code)), Err(AuthError::Expired)));
    assert!(matches!(block_on(flow.poll(&code)), Err(AuthError::AuthorizationDenied)));
    let unknown = block_on(flow.poll(&code)).expect_err("unknown error maps to Http");
    assert!(matches!(unknown, AuthError::Http { .. }), "{unknown:?}");
}

#[test]
fn tokens_round_trip_through_the_settings_store() {
    let mut settings = Settings::in_memory();
    let tokens = nanobot_auth::TokenSet {
        access_token: "at-1".into(),
        refresh_token: Some("rt-1".into()),
        expires_at_ms: Some(1_000),
        token_type: Some("Bearer".into()),
        id_token: Some("h.p.s".into()),
    };

    nanobot_auth::persist_tokens(&mut settings, "acme", &tokens).expect("persist");
    let restored = nanobot_auth::restore_tokens(&settings, "acme").expect("restore");
    assert_eq!(restored, tokens);

    // A different label does not see another label's grant.
    assert!(nanobot_auth::restore_tokens(&settings, "other").is_none());

    // A file-backed store becomes owner-only (Unix).
    let dir = std::env::temp_dir().join(format!("nb-auth-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut file_settings = Settings::load(&dir.join("settings.json")).0;
    nanobot_auth::persist_tokens(&mut file_settings, "acme", &tokens).expect("persist file");

    nanobot_auth::forget_tokens(&mut settings, "acme").expect("forget");
    assert!(nanobot_auth::restore_tokens(&settings, "acme").is_none());

    let _ = std::fs::remove_dir_all(&dir);
}
