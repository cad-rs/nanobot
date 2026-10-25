//! The OAuth2 device flow (RFC 8628): the authorization shape a desktop
//! harness can actually run. There is no loopback redirect to bind and no
//! embedded browser — the flow shows the user a short code, the user opens
//! the verification URL anywhere, and the harness polls the token endpoint
//! until the authorization lands.
//!
//! Split of responsibilities (the seam the framework keeps):
//!
//! - [`DeviceFlow`] — the transport. `begin` / `poll` / `refresh` are plain
//!   `async fn` over reqwest; the caller's executor drives them and the
//!   caller owns the polling loop (and its sleeping), because only the
//!   caller knows its timer surface (a gpui shell, a CLI, a test).
//! - [`OAuth2Provider`] — the [`AccountProvider`](nanobot_boot::AccountProvider)
//!   implementation over a token cache. Its methods **never touch the
//!   network**: `account()` and `check()` answer from local state, so the
//!   synchronous harness seam stays synchronous. The app layer drives
//!   `refresh_now` on its own schedule (a shell's poll loop is exactly the
//!   right driver) and feeds results back through [`OAuth2Provider::adopt`].

use std::sync::Mutex;

use nanobot_boot::{Account, AccountProvider, EntitlementDecision, Settings};
use serde_json::Value;

use crate::error::AuthError;

/// The endpoints and identity of one OAuth2 client. Plain data: the product
/// fills it from its own configuration (a provider block, a const).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuth2Config {
    pub client_id: String,
    /// The device-authorization endpoint (`/oauth/device/authorize`-shaped).
    pub device_auth_url: String,
    /// The token endpoint.
    pub token_url: String,
    pub scopes: Vec<String>,
    /// Some providers require an audience parameter (Auth0-style).
    pub audience: Option<String>,
}

/// What `begin` hands back: what the user sees and what the harness polls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: String,
    /// The short code the user types at `verification_uri`.
    pub user_code: String,
    pub verification_uri: String,
    /// A URL that pre-fills the code, when the provider offers one.
    pub verification_uri_complete: Option<String>,
    /// When the code stops being valid (ms from now, provider-declared).
    pub expires_in_ms: u64,
    /// The minimum poll interval the provider demands (ms).
    pub interval_ms: u64,
}

/// The outcome of one [`DeviceFlow::poll`] round. The caller loops:
/// [`PollOutcome::Authorized`] ends the flow, the pending shapes mean "sleep
/// `retry_after_ms`, then poll again", and the errors mean stop.
#[derive(Clone, Debug)]
pub enum PollOutcome {
    Authorized(TokenSet),
    /// The user has not finished yet.
    Pending { retry_after_ms: u64 },
    /// The provider demanded a slower cadence (`slow_down`). Honour it: the
    /// next poll must wait at least this much longer.
    SlowDown { retry_after_ms: u64 },
}

/// One grant's tokens. `id_token` is kept for the claims it carries
/// (display identity); the framework never validates it — that happened at
/// the TLS-protected token endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// When the access token stops working (ms since the unix epoch).
    pub expires_at_ms: Option<u64>,
    pub token_type: Option<String>,
    pub id_token: Option<String>,
}

impl TokenSet {
    /// The JWT `id_token`'s payload claims, decoded without verification
    /// (display only; see the type docs).
    pub fn id_token_claims(&self) -> Option<Value> {
        let id_token = self.id_token.as_deref()?;
        let payload = id_token.split('.').nth(1)?;
        jwt_payload_json(payload)
    }
}

/// A very small JWT-claim decoder: base64url (unpadded) to bytes to JSON.
fn jwt_payload_json(payload: &str) -> Option<Value> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = Vec::with_capacity(payload.len() * 3 / 4);
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    for ch in payload.trim_end_matches('=').bytes() {
        let value = TABLE.iter().position(|c| *c == ch)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    serde_json::from_slice(&out).ok()
}

/// The RFC 8628 transport. Cheap to clone; all methods take `&self`.
#[derive(Clone)]
pub struct DeviceFlow {
    config: OAuth2Config,
    http: reqwest::Client,
}

impl DeviceFlow {
    pub fn new(config: OAuth2Config) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Starts the flow: the provider mint a device code.
    pub async fn begin(&self) -> Result<DeviceCode, AuthError> {
        let mut form: Vec<(String, String)> =
            vec![("client_id".into(), self.config.client_id.clone())];
        if !self.config.scopes.is_empty() {
            form.push(("scope".into(), self.config.scopes.join(" ")));
        }
        if let Some(audience) = &self.config.audience {
            form.push(("audience".into(), audience.clone()));
        }
        let response = self
            .http
            .post(&self.config.device_auth_url)
            .form(&form)
            .send()
            .await
            .map_err(|_| AuthError::Protocol("device authorization request failed".into()))?;
        finish(
            response,
            |body| {
                Some(DeviceCode {
                    device_code: string_field(body, "device_code")?,
                    user_code: string_field(body, "user_code")?,
                    verification_uri: string_field(body, "verification_uri")?,
                    verification_uri_complete: string_field(body, "verification_uri_complete"),
                    expires_in_ms: (float_field(body, "expires_in").unwrap_or(600.0) * 1000.0)
                        as u64,
                    interval_ms: (float_field(body, "interval").unwrap_or(5.0) * 1000.0) as u64,
                })
            },
            "device authorization",
        )
        .await
    }

    /// One poll round against the token endpoint. The caller sleeps
    /// `retry_after_ms` between rounds ([`PollOutcome::Pending`] /
    /// [`PollOutcome::SlowDown`]) — the loop and its timer belong to the
    /// caller.
    pub async fn poll(&self, code: &DeviceCode) -> Result<PollOutcome, AuthError> {
        let response = self
            .http
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", code.device_code.as_str()),
                ("client_id", self.config.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(|_| AuthError::Protocol("token poll request failed".into()))?;

        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        let json: Value = serde_json::from_str(body.trim())
            .map_err(|_| AuthError::Protocol(format!("token poll body: {body:.120}")))?;

        if status == 200 {
            if let Some(access) = string_field(&json, "access_token") {
                let expires_in = float_field(&json, "expires_in");
                return Ok(PollOutcome::Authorized(TokenSet {
                    access_token: access,
                    refresh_token: string_field(&json, "refresh_token"),
                    expires_at_ms: expires_in
                        .map(|secs| now_ms() + (secs * 1000.0) as u64),
                    token_type: string_field(&json, "token_type"),
                    id_token: string_field(&json, "id_token"),
                }));
            }
            return Err(AuthError::Protocol("200 without access_token".into()));
        }

        match string_field(&json, "error").as_deref() {
            Some("authorization_pending") => Ok(PollOutcome::Pending {
                retry_after_ms: code.interval_ms,
            }),
            Some("slow_down") => Ok(PollOutcome::SlowDown {
                retry_after_ms: code.interval_ms + 5_000,
            }),
            Some("expired_token") => Err(AuthError::Expired),
            Some("access_denied") => Err(AuthError::AuthorizationDenied),
            Some(other) => Err(AuthError::Http { status, body: other.to_string() }),
            None => Err(AuthError::Protocol(format!("token poll status {status}: {body:.120}"))),
        }
    }

    /// Exchanges a refresh token for a fresh [`TokenSet`].
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenSet, AuthError> {
        let response = self
            .http
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", self.config.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(|_| AuthError::Protocol("refresh request failed".into()))?;
        finish(
            response,
            |body| {
                Some(TokenSet {
                    access_token: string_field(body, "access_token")?,
                    refresh_token: string_field(body, "refresh_token"),
                    expires_at_ms: float_field(body, "expires_in")
                        .map(|secs| now_ms() + (secs * 1000.0) as u64),
                    token_type: string_field(body, "token_type"),
                    id_token: string_field(body, "id_token"),
                })
            },
            "refresh",
        )
        .await
    }
}

/// 2xx → parse with `parse`; otherwise map the OAuth2 `error` field onto the
/// vocabulary. Shared by `begin` and `refresh` (poll needs the pending
/// shapes, so it inlines).
async fn finish<T>(
    response: reqwest::Response,
    parse: impl Fn(&Value) -> Option<T>,
    what: &'static str,
) -> Result<T, AuthError> {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    let json: Value = serde_json::from_str(body.trim())
        .map_err(|_| AuthError::Protocol(format!("{what} body: {body:.120}")))?;
    if (200..300).contains(&status) {
        parse(&json).ok_or_else(|| AuthError::Protocol(format!("{what} response incomplete")))
    } else {
        match string_field(&json, "error").as_deref() {
            Some("expired_token") => Err(AuthError::Expired),
            Some("access_denied") => Err(AuthError::AuthorizationDenied),
            _ => Err(AuthError::Http {
                status,
                body: body.chars().take(200).collect(),
            }),
        }
    }
}

fn string_field(json: &Value, key: &str) -> Option<String> {
    json.get(key)?.as_str().map(str::to_string)
}

fn float_field(json: &Value, key: &str) -> Option<f64> {
    json.get(key)?.as_f64()
}

fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The [`AccountProvider`] over a device-flow token cache. Construction is
/// network-free; a fresh provider is a signed-out provider.
pub struct OAuth2Provider {
    flow: DeviceFlow,
    tokens: Mutex<Option<TokenSet>>,
}

impl OAuth2Provider {
    pub fn new(config: OAuth2Config) -> Self {
        Self {
            flow: DeviceFlow::new(config),
            tokens: Mutex::new(None),
        }
    }

    /// The transport, for the app layer's sign-in loop (`begin` → show the
    /// user the code → `poll` until [`PollOutcome::Authorized`]) and its
    /// refresh schedule.
    pub fn flow(&self) -> &DeviceFlow {
        &self.flow
    }

    /// Caches a grant — after a completed sign-in, or at launch when the
    /// tokens were restored from the settings store.
    pub fn adopt(&self, tokens: TokenSet) {
        *self.tokens.lock().expect("token cache") = Some(tokens);
    }

    /// The cached grant, for persistence between runs.
    pub fn current_tokens(&self) -> Option<TokenSet> {
        self.tokens.lock().expect("token cache").clone()
    }
}

impl AccountProvider for OAuth2Provider {
    /// The identity from the cached `id_token` claims (`sub`, `name`,
    /// `email`), or a minimal placeholder account when the provider issued
    /// no id_token. `None` while signed out.
    fn account(&self) -> Option<Account> {
        let tokens = self.tokens.lock().expect("token cache").clone()?;
        let claims = tokens.id_token_claims().unwrap_or_default();
        let subject = claims
            .get("sub")
            .and_then(Value::as_str)
            .unwrap_or("oauth")
            .to_string();
        let display = claims
            .get("name")
            .or_else(|| claims.get("email"))
            .and_then(Value::as_str)
            .unwrap_or(&subject)
            .to_string();
        Some(Account::new(subject, display).with_expiry_opts(tokens.expires_at_ms))
    }

    /// Allowed while a non-expired grant is cached. No network, ever.
    fn check(&self, _capability: &str) -> EntitlementDecision {
        let Some(tokens) = self.tokens.lock().expect("token cache").clone() else {
            return EntitlementDecision::Denied {
                reason: "not signed in".to_string(),
            };
        };
        if let Some(expires_at) = tokens.expires_at_ms {
            if now_ms() >= expires_at {
                return EntitlementDecision::Denied {
                    reason: format!("access token expired at {expires_at} ms"),
                };
            }
        }
        EntitlementDecision::Allowed
    }
}

/// Settings keys the token cache persists under, keyed by an account label
/// (`auth.oauth.<label>.<field>`).
mod keys {
    pub const PREFIX: &str = "auth.oauth";
    pub const ACCESS: &str = "access";
    pub const REFRESH: &str = "refresh";
    pub const EXPIRES: &str = "expires_at_ms";
    pub const TOKEN_TYPE: &str = "token_type";
    pub const ID_TOKEN: &str = "id_token";
}

use keys::PREFIX;

/// Persists the grant through the harness's settings store (opt-in: only the
/// app layer knows whether a machine may keep secrets at rest). On Unix the
/// settings file is tightened to owner-only afterwards.
pub fn persist_tokens(
    settings: &mut Settings,
    label: &str,
    tokens: &TokenSet,
) -> Result<(), String> {
    settings.set(
        &format!("{PREFIX}.{label}.{}", keys::ACCESS),
        Value::String(tokens.access_token.clone()),
    )?;
    if let Some(refresh) = &tokens.refresh_token {
        settings.set(
            &format!("{PREFIX}.{label}.{}", keys::REFRESH),
            Value::String(refresh.clone()),
        )?;
    }
    if let Some(expires) = tokens.expires_at_ms {
        settings.set(
            &format!("{PREFIX}.{label}.{}", keys::EXPIRES),
            Value::from(expires),
        )?;
    }
    if let Some(kind) = &tokens.token_type {
        settings.set(
            &format!("{PREFIX}.{label}.{}", keys::TOKEN_TYPE),
            Value::String(kind.clone()),
        )?;
    }
    if let Some(id_token) = &tokens.id_token {
        settings.set(
            &format!("{PREFIX}.{label}.{}", keys::ID_TOKEN),
            Value::String(id_token.clone()),
        )?;
    }
    tighten_file_mode(settings.path());
    Ok(())
}

/// Restores what [`persist_tokens`] stored, `None` when nothing is there.
pub fn restore_tokens(settings: &Settings, label: &str) -> Option<TokenSet> {
    let field = |name: &str| settings.get(&format!("{PREFIX}.{label}.{name}")).cloned();
    let access = field(keys::ACCESS)?.as_str()?.to_string();
    Some(TokenSet {
        access_token: access,
        refresh_token: field(keys::REFRESH)
            .and_then(|v| v.as_str().map(str::to_string)),
        expires_at_ms: field(keys::EXPIRES).and_then(|v| v.as_u64()),
        token_type: field(keys::TOKEN_TYPE)
            .and_then(|v| v.as_str().map(str::to_string)),
        id_token: field(keys::ID_TOKEN).and_then(|v| v.as_str().map(str::to_string)),
    })
}

/// Removes a persisted grant (sign-out).
pub fn forget_tokens(settings: &mut Settings, label: &str) -> Result<(), String> {
    for name in [keys::ACCESS, keys::REFRESH, keys::EXPIRES, keys::TOKEN_TYPE, keys::ID_TOKEN] {
        settings.remove(&format!("{PREFIX}.{label}.{name}"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn tighten_file_mode(path: Option<&std::path::Path>) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(path) = path {
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            let _ = std::fs::set_permissions(path, permissions);
        }
    }
}
#[cfg(not(unix))]
fn tighten_file_mode(_path: Option<&std::path::Path>) {}

/// Small helper so [`OAuth2Provider::account`] reads flat.
trait AccountExpiry {
    fn with_expiry_opts(self, expires_at_ms: Option<u64>) -> Self;
}
impl AccountExpiry for Account {
    fn with_expiry_opts(self, expires_at_ms: Option<u64>) -> Self {
        match expires_at_ms {
            Some(ms) => self.with_expiry_ms(ms),
            None => self,
        }
    }
}
