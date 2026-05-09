//! Short-lived flash cookie ("just deleted X") plumbing.

use tower_cookies::cookie::SameSite;
use tower_cookies::{Cookie, Cookies};

const COOKIE: &str = "tw1337_flash";
const TTL_SECS: i64 = 60;

/// Set a flash message that the next page load will display once and clear.
pub fn set(cookies: &Cookies, msg: &str) {
    cookies.add(
        Cookie::build((COOKIE, msg.to_owned()))
            .path("/")
            .secure(true)
            .same_site(SameSite::Lax)
            .max_age(time::Duration::seconds(TTL_SECS))
            .build(),
    );
}

/// Read + immediately clear the flash cookie.
pub fn take(cookies: &Cookies) -> Option<String> {
    let value = cookies.get(COOKIE).map(|c| c.value().to_owned());
    if value.is_some() {
        cookies.remove(Cookie::build(COOKIE).path("/").build());
    }
    value
}
