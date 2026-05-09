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
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    RedirectUrl, Scope, TokenResponse as _, TokenUrl,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use tower_cookies::cookie::SameSite;
use tower_cookies::{Cookie, Cookies};

use crate::auth::mod_check::{ModCheckOutcome, check_is_mod};
use crate::error::WebError;
use crate::state::WebState;

const SID_COOKIE: &str = "tw1337_sid";
const CSRF_COOKIE: &str = "tw1337_csrf";
const OAUTH_STATE_COOKIE: &str = "tw1337_oauth_state";

/// Fully-configured `BasicClient` (auth/token/redirect endpoints all set).
type ConfiguredClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

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
        let basic = BasicClient::new(ClientId::new(client_id.to_owned()))
            .set_client_secret(ClientSecret::new(client_secret.expose_secret().to_owned()))
            .set_auth_uri(AuthUrl::new(
                "https://id.twitch.tv/oauth2/authorize".into(),
            )?)
            .set_token_uri(TokenUrl::new("https://id.twitch.tv/oauth2/token".into())?)
            .set_redirect_uri(RedirectUrl::new(redirect)?);
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
    let bytes = resp.bytes().await?.to_vec();

    let mut http_resp = http::Response::builder().status(status.as_u16());
    for (name, value) in headers.iter() {
        http_resp = http_resp.header(name.as_str(), value.as_bytes());
    }
    http_resp
        .body(bytes)
        .map_err(|e| OAuthHttpError::Header(e.to_string()))
}

pub fn auth_router() -> Router<WebState> {
    Router::new()
        .route("/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/logout", post(logout))
}

async fn login(State(state): State<WebState>, cookies: Cookies) -> Response {
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
    let csrf_for_url = csrf.clone();
    // `moderation:read` lets us check the helix moderators list against the
    // user's own access token in the callback, so the bot's IRC token does
    // not need that scope.
    let (auth_url, _) = state
        .oauth
        .basic
        .authorize_url(move || csrf_for_url.clone())
        .add_scope(Scope::new("user:read:email".to_owned()))
        .add_scope(Scope::new("moderation:read".to_owned()))
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
        .map_err(|e| WebError::OAuthExchange(e.to_string()))?;

    let user_token = token.access_token().secret().to_owned();
    let me = fetch_caller_user(&state, &user_token)
        .await
        .map_err(|e| WebError::OAuthExchange(format!("user lookup: {e}")))?;

    // Initial mod check uses the user's own access token (granted
    // `moderation:read` via OAuth scope), so the bot's IRC token does not
    // need extra scopes. The require_mod middleware re-checks via the bot
    // token; on failure there it log+admits to avoid lockout.
    match crate::auth::mod_check::check_is_mod_with_token(
        &state,
        &me.id,
        &user_token,
        &state.broadcaster_id,
        &state.hidden_admins,
    )
    .await
    .map_err(|e| WebError::OAuthExchange(format!("mod check: {e}")))?
    {
        ModCheckOutcome::Allow => {}
        ModCheckOutcome::Deny => {
            tracing::info!(target: "twitch_1337_web", user_id=%me.id, user_login=%me.login, action="login", result="denied");
            return Err(WebError::Forbidden);
        }
    }

    let (sid, csrf_value) = state
        .sessions
        .insert(me.id.clone(), me.login.clone())
        .map_err(WebError::Internal)?;
    let csrf_value_hex = hex::encode(csrf_value);

    cookies.add(
        Cookie::build((SID_COOKIE, sid))
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Lax)
            .path("/")
            .build(),
    );
    cookies.add(
        Cookie::build((CSRF_COOKIE, csrf_value_hex))
            .secure(true)
            .same_site(SameSite::Lax)
            .path("/")
            .build(),
    );

    tracing::info!(target: "twitch_1337_web", user_id=%me.id, user_login=%me.login, action="login", result="ok");
    Ok(Redirect::to("/").into_response())
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
    let resp: Resp = state
        .oauth
        .http
        .get("https://api.twitch.tv/helix/users")
        .bearer_auth(access_token)
        .header("Client-Id", state.client_id.expose_secret())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    resp.data
        .into_iter()
        .next()
        .ok_or_else(|| eyre!("empty user list"))
}

pub async fn require_mod(
    State(state): State<WebState>,
    cookies: Cookies,
    mut req: Request,
    next: Next,
) -> Result<Response, WebError> {
    let sid = cookies.get(SID_COOKIE).ok_or(WebError::Unauthenticated)?;
    let session = state
        .sessions
        .get_and_touch(sid.value())
        .ok_or(WebError::Unauthenticated)?;

    let now = state.clock.now();
    let elapsed = now
        .signed_duration_since(session.last_mod_check)
        .to_std()
        .unwrap_or_default();
    if elapsed > state.config.mod_check_refresh {
        match check_is_mod(
            state.helix.as_ref(),
            &session.user_id,
            &state.broadcaster_id,
            &state.hidden_admins,
        )
        .await
        {
            Ok(ModCheckOutcome::Allow) => state.sessions.record_mod_check(sid.value()),
            Ok(ModCheckOutcome::Deny) => {
                state.sessions.drop_session(sid.value());
                tracing::info!(target: "twitch_1337_web", user_id=%session.user_id, action="mod_recheck", result="denied");
                return Err(WebError::Forbidden);
            }
            Err(e) => {
                tracing::warn!(
                    target: "twitch_1337_web",
                    error = ?e,
                    "mod refresh failed; admitting on stale check"
                );
            }
        }
    }

    req.extensions_mut().insert(session);
    Ok(next.run(req).await)
}
