//! Role-gate decision: hidden_admins → broadcaster → helix moderators.
//!
//! Hidden admins (configured in `[twitch].hidden_admins`) short-circuit the
//! helix lookup so a debugging account always retains access. The broadcaster
//! id is checked next as a fast path. Otherwise we follow the moderator list.

use secrecy::ExposeSecret as _;

use crate::helix::HelixClient;
use crate::state::WebState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    Allow,
    Deny,
}

/// Hidden_admins / broadcaster shortcuts shared by both check variants. Returns
/// `Some(Allow)` iff a shortcut applies; `None` means the helix lookup runs.
fn shortcut(user_id: &str, broadcaster_id: &str, hidden_admins: &[String]) -> Option<GateOutcome> {
    if hidden_admins.iter().any(|s| s == user_id) || user_id == broadcaster_id {
        Some(GateOutcome::Allow)
    } else {
        None
    }
}

pub async fn check_is_mod(
    helix: &dyn HelixClient,
    user_id: &str,
    broadcaster_id: &str,
    hidden_admins: &[String],
) -> eyre::Result<GateOutcome> {
    if let Some(o) = shortcut(user_id, broadcaster_id, hidden_admins) {
        return Ok(o);
    }
    if helix.is_moderator(broadcaster_id, user_id).await? {
        Ok(GateOutcome::Allow)
    } else {
        Ok(GateOutcome::Deny)
    }
}

/// Variant used during the OAuth callback. Asks Twitch which channels the
/// user moderates (scope `user:read:moderated_channels` on the user token)
/// and checks `broadcaster_id` against that list — the
/// `helix/moderation/moderators` endpoint can't be used here because it
/// requires the bearer to *be* the broadcaster.
pub async fn check_is_mod_with_token(
    state: &WebState,
    user_id: &str,
    user_access_token: &str,
    broadcaster_id: &str,
    hidden_admins: &[String],
) -> eyre::Result<GateOutcome> {
    if let Some(o) = shortcut(user_id, broadcaster_id, hidden_admins) {
        return Ok(o);
    }
    if is_moderator_with_user_token(user_id, user_access_token, broadcaster_id, state).await? {
        Ok(GateOutcome::Allow)
    } else {
        Ok(GateOutcome::Deny)
    }
}

async fn is_moderator_with_user_token(
    user_id: &str,
    access_token: &str,
    broadcaster_id: &str,
    state: &WebState,
) -> eyre::Result<bool> {
    crate::helix::user_moderates_channel(
        &state.oauth.http,
        "https://api.twitch.tv",
        state.client_id.expose_secret(),
        access_token,
        user_id,
        broadcaster_id,
        "helix moderated channels (user token)",
    )
    .await
}

/// Allow iff `user_id` appears in `allowlist`.
pub fn check_in_allowlist(user_id: &str, allowlist: &[String]) -> GateOutcome {
    if allowlist.iter().any(|id| id == user_id) {
        GateOutcome::Allow
    } else {
        GateOutcome::Deny
    }
}
