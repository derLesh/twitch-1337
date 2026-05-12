//! OAuth login/callback/logout handlers + mod-gate / CSRF middlewares.
//!
//! `OAuthCtx` wraps the configured `BasicClient`. Login redirects to twitch
//! with a freshly issued `tw1337_oauth_state` cookie. The callback verifies
//! the state, exchanges the auth code, looks up the caller via helix,
//! enforces the mod check, and finally drops `tw1337_sid` (HttpOnly) and
//! `tw1337_csrf` (JS-readable, matched against the session secret).
//!
//! axum can't read a request body twice, so each mutating handler validates
//! its `_csrf` form field via [`crate::auth::csrf::verify`] against the
//! session's csrf value. The header path (HTMX delete) is the only mutation
//! that needs no body parse — handlers read `X-Csrf-Token` directly.

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use eyre::{Result, WrapErr as _, eyre};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet,
    EndpointSet, RedirectUrl, Scope, TokenResponse as _, TokenUrl,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use tower_cookies::cookie::SameSite;
use tower_cookies::{Cookie, Cookies, Key};

use crate::auth::role_check::{GateOutcome, check_in_allowlist, check_is_mod};
use crate::error::WebError;
use crate::state::WebState;

pub const SID_COOKIE: &str = "tw1337_sid";
pub const CSRF_COOKIE: &str = "tw1337_csrf";
const OAUTH_STATE_COOKIE: &str = "tw1337_oauth_state";
/// Short-lived cookie that stashes the original requested path captured by
/// `require_mod` into `?next=`. Consumed (and cleared) by the callback.
const NEXT_COOKIE: &str = "tw1337_next";

/// Fully-configured `BasicClient` (auth/token/redirect endpoints all set).
type ConfiguredClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Set the signed sid + csrf cookies that mark a logged-in session. Shared
/// between the OAuth callback (`secure = true`) and the dev-login bypass
/// (`secure = false`, since dev runs over plain http on localhost).
pub(crate) fn issue_session_cookies(
    cookies: &Cookies,
    key: &Key,
    sid: String,
    csrf_bytes: &[u8; 32],
    secure: bool,
) {
    let signed = cookies.signed(key);
    signed.add(
        Cookie::build((SID_COOKIE, sid))
            .http_only(true)
            .secure(secure)
            .same_site(SameSite::Lax)
            .path("/")
            .build(),
    );
    signed.add(
        Cookie::build((CSRF_COOKIE, crate::auth::csrf::encode(csrf_bytes)))
            .secure(secure)
            .same_site(SameSite::Lax)
            .path("/")
            .build(),
    );
}

pub struct OAuthCtx {
    pub basic: ConfiguredClient,
    /// Owned reqwest client used for token exchanges. Disables redirects to
    /// avoid SSRF (oauth2 docs explicitly recommend this). Workspace pins
    /// `reqwest = 0.13` while oauth2 v5 pins `reqwest = 0.12`; the closure
    /// adapter `oauth_http_call` translates between the workspace reqwest
    /// and oauth2's `AsyncHttpClient` blanket impl over `Fn(HttpRequest)`.
    pub http: reqwest::Client,
}

impl OAuthCtx {
    pub fn new(client_id: &str, client_secret: &SecretString, public_url: &str) -> Result<Self> {
        let redirect = format!("{}/auth/callback", public_url.trim_end_matches('/'));
        // Twitch's token endpoint only reads `client_id` / `client_secret` from
        // the form body and ignores HTTP Basic auth, so override oauth2 v5's
        // BasicAuth default — otherwise Twitch returns
        // `{"status":400,"message":"missing client id"}`.
        let basic = BasicClient::new(ClientId::new(client_id.to_owned()))
            .set_client_secret(ClientSecret::new(client_secret.expose_secret().to_owned()))
            .set_auth_uri(AuthUrl::new(
                "https://id.twitch.tv/oauth2/authorize".into(),
            )?)
            .set_token_uri(TokenUrl::new("https://id.twitch.tv/oauth2/token".into())?)
            .set_redirect_uri(RedirectUrl::new(redirect)?)
            .set_auth_type(AuthType::RequestBody);
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .wrap_err("oauth http client")?;
        Ok(Self { basic, http })
    }
}

/// Errors returned by the oauth2-bridging http closure.
#[derive(Debug, thiserror::Error)]
pub enum OAuthHttpError {
    #[error("reqwest: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid header: {0}")]
    Header(String),
}

/// Adapt the workspace `reqwest::Client` (0.13) to the closure form
/// `Fn(HttpRequest) -> Future<Output = Result<HttpResponse, _>>` so it
/// satisfies `oauth2::AsyncHttpClient` despite oauth2 internally depending
/// on `reqwest = 0.12`.
async fn oauth_http_call(
    client: reqwest::Client,
    req: oauth2::HttpRequest,
) -> Result<oauth2::HttpResponse, OAuthHttpError> {
    let (parts, body) = req.into_parts();
    let url = parts.uri.to_string();
    let mut builder = client.request(parts.method.clone(), &url).body(body);
    for (name, value) in parts.headers.iter() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let resp = builder.send().await?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = normalize_twitch_scope(resp.bytes().await?.to_vec());

    let mut http_resp = http::Response::builder().status(status.as_u16());
    for (name, value) in headers.iter() {
        http_resp = http_resp.header(name.as_str(), value.as_bytes());
    }
    http_resp
        .body(bytes)
        .map_err(|e| OAuthHttpError::Header(e.to_string()))
}

/// Twitch returns `scope` as a JSON array (`["a","b"]`), but RFC 6749 — and
/// therefore oauth2 v5's `StandardTokenResponse` deserializer — expects a
/// space-separated string. Rewrite arrays to strings so token parsing
/// succeeds. No-op on non-JSON, non-object, or already-string shapes.
fn normalize_twitch_scope(body: Vec<u8>) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(obj) = value.as_object_mut() else {
        return body;
    };
    let Some(arr) = obj.get("scope").and_then(serde_json::Value::as_array) else {
        return body;
    };
    let joined = arr
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    obj.insert("scope".to_owned(), serde_json::Value::String(joined));
    serde_json::to_vec(&value).unwrap_or(body)
}

/// Replace values of known credential-bearing JSON fields with `<redacted>`
/// so error logs that include a response body never leak access/refresh
/// tokens. No-op on non-JSON or non-object bodies.
fn redact_token_body(body: &[u8]) -> String {
    let mut value = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(v) => v,
        Err(_) => return String::from_utf8_lossy(body).into_owned(),
    };
    let Some(obj) = value.as_object_mut() else {
        return String::from_utf8_lossy(body).into_owned();
    };
    for key in ["access_token", "refresh_token", "id_token"] {
        if let Some(slot) = obj.get_mut(key) {
            *slot = serde_json::Value::String("<redacted>".into());
        }
    }
    value.to_string()
}

pub fn auth_router() -> Router<WebState> {
    Router::new()
        .route("/login", get(login_landing))
        .route("/auth/start", get(auth_start))
        .route("/auth/callback", get(callback))
        .route("/logout", post(logout))
}

#[derive(Deserialize)]
struct LoginParams {
    next: Option<String>,
}

#[derive(askama::Template)]
#[template(path = "auth/login.html")]
struct LoginTpl {
    login_href: String,
}

/// Static landing page reached on first hit and after `/logout`. Without
/// this stop, redirecting straight to Twitch's `/authorize` would silently
/// re-authenticate via the still-active Twitch session and the user would
/// appear to "fail to log out".
async fn login_landing(Query(params): Query<LoginParams>) -> Response {
    let login_href = match params
        .next
        .as_deref()
        .filter(|p| crate::error::is_safe_redirect(p))
    {
        Some(next) => format!("/auth/start?next={}", urlencoding::encode(next)),
        None => "/auth/start".to_owned(),
    };
    use askama::Template as _;
    match (LoginTpl { login_href }).render() {
        Ok(body) => axum::response::Html(body).into_response(),
        Err(err) => {
            tracing::error!(target: "twitch_1337_web", ?err, "login template render failed");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
            )
                .into_response()
        }
    }
}

async fn auth_start(
    State(state): State<WebState>,
    Query(params): Query<LoginParams>,
    cookies: Cookies,
) -> Response {
    let csrf = CsrfToken::new_random();
    cookies.add(
        Cookie::build((OAUTH_STATE_COOKIE, csrf.secret().to_owned()))
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Lax)
            .path("/")
            .max_age(time::Duration::minutes(10))
            .build(),
    );
    if let Some(path) = params.next.as_deref()
        && crate::error::is_safe_redirect(path)
    {
        cookies.add(
            Cookie::build((NEXT_COOKIE, path.to_owned()))
                .http_only(true)
                .secure(true)
                .same_site(SameSite::Lax)
                .path("/")
                .max_age(time::Duration::minutes(10))
                .build(),
        );
    }
    let csrf_for_url = csrf.clone();
    // `user:read:moderated_channels` lets the callback ask Twitch which
    // channels the logging-in user moderates and check our broadcaster
    // against that list — `moderation:read` would only work if the
    // logging-in user IS the broadcaster.
    let (auth_url, _) = state
        .oauth
        .basic
        .authorize_url(move || csrf_for_url.clone())
        .add_scope(Scope::new("user:read:email".to_owned()))
        .add_scope(Scope::new("user:read:moderated_channels".to_owned()))
        .url();
    Redirect::to(auth_url.as_ref()).into_response()
}

#[derive(Deserialize)]
struct CallbackParams {
    code: String,
    state: String,
}

async fn callback(
    State(state): State<WebState>,
    Query(params): Query<CallbackParams>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let stored = cookies.get(OAUTH_STATE_COOKIE).ok_or(WebError::Forbidden)?;
    if stored.value() != params.state {
        return Err(WebError::Forbidden);
    }
    cookies.remove(Cookie::build(OAUTH_STATE_COOKIE).path("/").build());

    let http = state.oauth.http.clone();
    let http_call = move |req: oauth2::HttpRequest| {
        let client = http.clone();
        oauth_http_call(client, req)
    };
    let token = state
        .oauth
        .basic
        .exchange_code(AuthorizationCode::new(params.code))
        .request_async(&http_call)
        .await
        .map_err(|e| {
            // Twitch returns non-RFC-6749 error bodies that fail Parse; surface
            // the (token-redacted) raw body so logs show the upstream message
            // without leaking access/refresh tokens on success-shaped bodies.
            let msg = match &e {
                oauth2::RequestTokenError::Parse(_, body) => {
                    format!(
                        "token exchange (response body: {})",
                        redact_token_body(body)
                    )
                }
                _ => "token exchange".into(),
            };
            WebError::OAuthExchange(eyre::Report::new(e).wrap_err(msg))
        })?;

    let user_token = token.access_token().secret().to_owned();
    let me = fetch_caller_user(&state, &user_token)
        .await
        .map_err(|e| WebError::OAuthExchange(e.wrap_err("user lookup")))?;

    let role = match crate::auth::role_check::check_is_mod_with_token(
        &state,
        &me.id,
        &user_token,
        &state.broadcaster_id,
        &state.hidden_admins,
    )
    .await
    .map_err(|e| WebError::OAuthExchange(e.wrap_err("mod check")))?
    {
        GateOutcome::Allow => crate::auth::role::Role::Mod,
        GateOutcome::Deny => match check_in_allowlist(&me.id, &state.viewer_allowlist) {
            GateOutcome::Allow => crate::auth::role::Role::Viewer,
            GateOutcome::Deny => {
                tracing::info!(
                    target: "twitch_1337_web",
                    user_id = %me.id,
                    user_login = %me.login,
                    action = "login",
                    result = "denied",
                );
                return Err(WebError::Forbidden);
            }
        },
    };

    let (sid, csrf_value) = state
        .sessions
        .insert(me.id.clone(), me.login.clone(), role)
        .map_err(WebError::Internal)?;
    issue_session_cookies(&cookies, &state.signed_key, sid, &csrf_value, true);

    let next_path = cookies
        .get(NEXT_COOKIE)
        .map(|c| c.value().to_owned())
        .filter(|p| crate::error::is_safe_redirect(p))
        .unwrap_or_else(|| "/".to_owned());
    cookies.remove(Cookie::build(NEXT_COOKIE).path("/").build());

    tracing::info!(
        target: "twitch_1337_web",
        user_id = %me.id,
        user_login = %me.login,
        role = role.label(),
        next_path = %next_path,
        action = "login",
        result = "ok",
    );
    Ok(Redirect::to(&next_path).into_response())
}

#[derive(Deserialize)]
struct LogoutForm {
    #[serde(rename = "_csrf")]
    csrf: String,
}

async fn logout(
    State(state): State<WebState>,
    cookies: Cookies,
    axum::Form(form): axum::Form<LogoutForm>,
) -> Result<Response, WebError> {
    let sid = cookies
        .signed(&state.signed_key)
        .get(SID_COOKIE)
        .map(|c| c.value().to_owned())
        .ok_or(WebError::CsrfMismatch)?;
    let session = state
        .sessions
        .get_and_touch(&sid)
        .ok_or(WebError::CsrfMismatch)?;
    if !crate::auth::csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    state.sessions.drop_session(&sid);
    cookies.remove(Cookie::build(SID_COOKIE).path("/").build());
    cookies.remove(Cookie::build(CSRF_COOKIE).path("/").build());
    Ok(Redirect::to("/login").into_response())
}

async fn fetch_caller_user(
    state: &WebState,
    access_token: &str,
) -> Result<crate::helix::HelixUser> {
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<crate::helix::HelixUser>,
    }
    let resp = state
        .oauth
        .http
        .get("https://api.twitch.tv/helix/users")
        .bearer_auth(access_token)
        .header("Client-Id", state.client_id.expose_secret())
        .send()
        .await
        .wrap_err("helix /users request send")?;
    let status = resp.status();
    let resp = resp
        .error_for_status()
        .wrap_err_with(|| format!("helix /users returned {status}"))?;
    let parsed: Resp = resp.json().await.wrap_err("helix /users decode")?;
    parsed
        .data
        .into_iter()
        .next()
        .ok_or_else(|| eyre!("helix /users returned empty data array"))
}

pub async fn viewer_method_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, WebError> {
    use axum::http::Method;
    match *req.method() {
        Method::GET | Method::HEAD => Ok(next.run(req).await),
        _ => Err(WebError::MethodNotAllowed),
    }
}

pub async fn require_role(
    min: crate::auth::role::Role,
    State(state): State<WebState>,
    cookies: Cookies,
    mut req: Request,
    next: Next,
) -> Result<Response, WebError> {
    let captured_next = req.uri().path_and_query().map(|pq| pq.as_str().to_owned());
    let unauth = || WebError::Unauthenticated {
        next: captured_next.clone(),
    };

    let sid_cookie = cookies
        .signed(&state.signed_key)
        .get(SID_COOKIE)
        .ok_or_else(unauth)?;
    let session = state
        .sessions
        .get_and_touch(sid_cookie.value())
        .ok_or_else(unauth)?;

    if session.role < min {
        return Err(WebError::Forbidden);
    }

    let now = state.clock.now();
    let elapsed = now
        .signed_duration_since(session.last_role_check)
        .to_std()
        .unwrap_or_default();
    if elapsed > state.config.role_check_refresh {
        let outcome: eyre::Result<GateOutcome> = match session.role {
            crate::auth::role::Role::Mod => {
                check_is_mod(
                    state.helix.as_ref(),
                    &session.user_id,
                    &state.broadcaster_id,
                    state.hidden_admins.as_ref(),
                )
                .await
            }
            crate::auth::role::Role::Viewer => Ok(check_in_allowlist(
                &session.user_id,
                &state.viewer_allowlist,
            )),
        };
        match outcome {
            Ok(GateOutcome::Allow) => state.sessions.record_role_check(sid_cookie.value()),
            Ok(GateOutcome::Deny) => {
                state.sessions.drop_session(sid_cookie.value());
                tracing::info!(
                    target: "twitch_1337_web",
                    user_id = %session.user_id,
                    role = session.role.label(),
                    action = "role_recheck",
                    result = "denied",
                );
                return Err(WebError::Forbidden);
            }
            Err(e) => {
                tracing::warn!(
                    target: "twitch_1337_web",
                    error = ?e,
                    "role refresh failed; admitting on stale check"
                );
            }
        }
    }

    req.extensions_mut().insert(session);
    Ok(next.run(req).await)
}

pub async fn require_mod(
    state: State<WebState>,
    cookies: Cookies,
    req: Request,
    next: Next,
) -> Result<Response, WebError> {
    require_role(crate::auth::role::Role::Mod, state, cookies, req, next).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_twitch_scope_rewrites_array_to_space_string() {
        let body = br#"{"access_token":"x","scope":["a","b"],"token_type":"bearer"}"#.to_vec();
        let out = normalize_twitch_scope(body);
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["scope"], serde_json::Value::String("a b".into()));
    }

    #[test]
    fn normalize_twitch_scope_passes_through_string_scope() {
        let body = br#"{"access_token":"x","scope":"a b","token_type":"bearer"}"#.to_vec();
        let out = normalize_twitch_scope(body.clone());
        // Re-parse rather than byte-compare; serialization order is not guaranteed.
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["scope"], serde_json::Value::String("a b".into()));
    }

    #[test]
    fn normalize_twitch_scope_passes_through_non_json() {
        let body = b"not json".to_vec();
        assert_eq!(normalize_twitch_scope(body.clone()), body);
    }

    #[test]
    fn normalize_twitch_scope_passes_through_missing_scope_field() {
        let body = br#"{"access_token":"x","token_type":"bearer"}"#.to_vec();
        assert_eq!(normalize_twitch_scope(body.clone()), body);
    }

    #[test]
    fn normalize_twitch_scope_handles_empty_array() {
        let body = br#"{"access_token":"x","scope":[],"token_type":"bearer"}"#.to_vec();
        let parsed: serde_json::Value =
            serde_json::from_slice(&normalize_twitch_scope(body)).unwrap();
        assert_eq!(parsed["scope"], serde_json::Value::String(String::new()));
    }

    #[test]
    fn normalize_twitch_scope_drops_non_string_array_elements() {
        // Twitch should never send numbers in scope, but pin the silent-drop
        // behavior of `filter_map(as_str)` so a future Twitch quirk surfaces
        // as a test diff rather than a parse failure on the live endpoint.
        let body = br#"{"scope":[1,"a",2,"b"]}"#.to_vec();
        let parsed: serde_json::Value =
            serde_json::from_slice(&normalize_twitch_scope(body)).unwrap();
        assert_eq!(parsed["scope"], serde_json::Value::String("a b".into()));
    }

    #[test]
    fn redact_token_body_replaces_known_credential_fields() {
        let body = br#"{"access_token":"secret","refresh_token":"rsecret","scope":["a"]}"#;
        let out = redact_token_body(body);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["access_token"], "<redacted>");
        assert_eq!(parsed["refresh_token"], "<redacted>");
        assert_eq!(parsed["scope"], serde_json::json!(["a"]));
    }

    #[test]
    fn redact_token_body_passes_through_error_body() {
        let body = br#"{"status":400,"message":"missing client id"}"#;
        let out = redact_token_body(body);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], 400);
        assert_eq!(parsed["message"], "missing client id");
    }

    #[test]
    fn redact_token_body_passes_through_non_json() {
        let body = b"plain text";
        assert_eq!(redact_token_body(body), "plain text");
    }

    #[test]
    fn redact_token_body_redacts_id_token() {
        let body = br#"{"id_token":"jwt.payload.sig","scope":"a"}"#;
        let parsed: serde_json::Value = serde_json::from_str(&redact_token_body(body)).unwrap();
        assert_eq!(parsed["id_token"], "<redacted>");
    }

    #[test]
    fn redact_token_body_passes_through_top_level_array() {
        let body = br#"["a","b"]"#;
        // Top-level non-object → fall through to lossy passthrough.
        assert_eq!(redact_token_body(body), r#"["a","b"]"#);
    }
}
