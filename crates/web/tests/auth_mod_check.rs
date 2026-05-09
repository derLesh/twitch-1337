use std::collections::HashMap;

use async_trait::async_trait;
use twitch_1337_web::auth::mod_check::{ModCheckOutcome, check_is_mod};
use twitch_1337_web::helix::{HelixClient, HelixUser};

struct FakeHelix {
    moderators: Vec<String>,
    users: HashMap<String, HelixUser>,
}

#[async_trait]
impl HelixClient for FakeHelix {
    async fn fetch_user_by_id(&self, id: &str) -> eyre::Result<Option<HelixUser>> {
        Ok(self.users.get(id).cloned())
    }
    async fn fetch_user_by_login(&self, login: &str) -> eyre::Result<Option<HelixUser>> {
        Ok(self.users.values().find(|u| u.login == login).cloned())
    }
    async fn is_moderator(&self, _broadcaster: &str, user_id: &str) -> eyre::Result<bool> {
        Ok(self.moderators.iter().any(|m| m == user_id))
    }
}

#[tokio::test]
async fn hidden_admin_short_circuits() {
    let helix = FakeHelix {
        moderators: vec![],
        users: HashMap::new(),
    };
    let outcome = check_is_mod(&helix, "12345", "200", &["12345".into()])
        .await
        .unwrap();
    assert_eq!(outcome, ModCheckOutcome::Allow);
}

#[tokio::test]
async fn broadcaster_short_circuits() {
    let helix = FakeHelix {
        moderators: vec![],
        users: HashMap::new(),
    };
    let outcome = check_is_mod(&helix, "200", "200", &[]).await.unwrap();
    assert_eq!(outcome, ModCheckOutcome::Allow);
}

#[tokio::test]
async fn moderator_path_admits() {
    let helix = FakeHelix {
        moderators: vec!["999".into()],
        users: HashMap::new(),
    };
    let outcome = check_is_mod(&helix, "999", "200", &[]).await.unwrap();
    assert_eq!(outcome, ModCheckOutcome::Allow);
}

#[tokio::test]
async fn non_mod_denied() {
    let helix = FakeHelix {
        moderators: vec![],
        users: HashMap::new(),
    };
    let outcome = check_is_mod(&helix, "555", "200", &[]).await.unwrap();
    assert_eq!(outcome, ModCheckOutcome::Deny);
}
