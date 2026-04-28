use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    User { subject_id: String },
    Lore,
    Pref { subject_id: String },
}

impl Scope {
    pub fn tag(&self) -> &'static str {
        match self {
            Scope::User { .. } => "user",
            Scope::Lore => "lore",
            Scope::Pref { .. } => "pref",
        }
    }

    pub fn subject_id(&self) -> Option<&str> {
        match self {
            Scope::User { subject_id } | Scope::Pref { subject_id } => Some(subject_id),
            Scope::Lore => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UserRole {
    Regular,
    Moderator,
    Broadcaster,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
    SelfClaim,
    ThirdParty,
    ModBroadcaster,
}

use twitch_irc::message::Badge;

/// Classify a Twitch user's role from their badge list
/// (typically `PrivmsgMessage::badges`). Broadcaster outranks moderator.
pub fn classify_role(badges: &[Badge]) -> UserRole {
    let mut role = UserRole::Regular;
    for b in badges {
        let rank = match b.name.as_str() {
            "broadcaster" => UserRole::Broadcaster,
            "moderator" => UserRole::Moderator,
            _ => continue,
        };
        if rank > role {
            role = rank;
        }
    }
    role
}

/// Returns true if the given `role` speaker may write to `scope` about `speaker_id`.
/// The caller is responsible for passing the scope constructed from whatever
/// `subject_id` the LLM requested.
pub fn is_write_allowed(role: UserRole, scope: &Scope, speaker_id: &str) -> bool {
    match scope {
        Scope::Pref { subject_id } => subject_id == speaker_id, // self-only, all roles
        Scope::User { subject_id } => match role {
            UserRole::Regular => subject_id == speaker_id,
            UserRole::Moderator | UserRole::Broadcaster => true,
        },
        Scope::Lore => matches!(role, UserRole::Moderator | UserRole::Broadcaster),
    }
}

/// Confidence seed for a successful write based on the trust relationship.
/// Invariant: self-writes are always `SelfClaim` regardless of role — mods
/// don't get the corroboration bonus when claiming facts about themselves.
pub fn trust_level_for(role: UserRole, scope: &Scope, speaker_id: &str) -> TrustLevel {
    match (role, scope) {
        (UserRole::Moderator | UserRole::Broadcaster, Scope::Lore) => TrustLevel::ModBroadcaster,
        (UserRole::Moderator | UserRole::Broadcaster, _)
            if scope.subject_id().is_some_and(|s| s != speaker_id) =>
        {
            TrustLevel::ModBroadcaster
        }
        _ => TrustLevel::SelfClaim,
    }
}

pub fn seed_confidence(level: TrustLevel) -> u8 {
    match level {
        TrustLevel::SelfClaim => 70,
        TrustLevel::ModBroadcaster => 90,
        TrustLevel::ThirdParty => 30, // rejected in practice; defined for completeness
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_user_serializes_with_subject_id() {
        let scope = Scope::User {
            subject_id: "12345".to_string(),
        };
        let s = ron::to_string(&scope).unwrap();
        let back: Scope = ron::from_str(&s).unwrap();
        assert_eq!(
            back,
            Scope::User {
                subject_id: "12345".to_string()
            }
        );
    }

    #[test]
    fn scope_lore_round_trips() {
        let scope = Scope::Lore;
        let s = ron::to_string(&scope).unwrap();
        let back: Scope = ron::from_str(&s).unwrap();
        assert_eq!(back, Scope::Lore);
    }

    #[test]
    fn user_role_broadcaster_outranks_moderator() {
        assert!(UserRole::Broadcaster > UserRole::Moderator);
        assert!(UserRole::Moderator > UserRole::Regular);
    }

    use twitch_irc::message::Badge;

    fn badges(names: &[&str]) -> Vec<Badge> {
        names
            .iter()
            .map(|b| Badge {
                name: (*b).to_string(),
                version: "1".to_string(),
            })
            .collect()
    }

    #[test]
    fn classify_role_regular_default() {
        assert_eq!(classify_role(&badges(&[])), UserRole::Regular);
    }

    #[test]
    fn classify_role_moderator_badge() {
        assert_eq!(classify_role(&badges(&["moderator"])), UserRole::Moderator);
    }

    #[test]
    fn classify_role_broadcaster_beats_moderator() {
        assert_eq!(
            classify_role(&badges(&["moderator", "broadcaster"])),
            UserRole::Broadcaster
        );
    }

    #[test]
    fn permission_matrix_table() {
        use Scope::*;
        use UserRole::*;

        let uid_a = "1".to_string();
        let uid_b = "2".to_string();

        // Regular
        assert!(is_write_allowed(
            Regular,
            &User {
                subject_id: uid_a.clone()
            },
            &uid_a
        ));
        assert!(!is_write_allowed(
            Regular,
            &User {
                subject_id: uid_b.clone()
            },
            &uid_a
        ));
        assert!(is_write_allowed(
            Regular,
            &Pref {
                subject_id: uid_a.clone()
            },
            &uid_a
        ));
        assert!(!is_write_allowed(
            Regular,
            &Pref {
                subject_id: uid_b.clone()
            },
            &uid_a
        ));
        assert!(!is_write_allowed(Regular, &Lore, &uid_a));

        // Moderator
        assert!(is_write_allowed(
            Moderator,
            &User {
                subject_id: uid_b.clone()
            },
            &uid_a
        ));
        assert!(is_write_allowed(Moderator, &Lore, &uid_a));
        // Pref stays self-only even for mod:
        assert!(is_write_allowed(
            Moderator,
            &Pref {
                subject_id: uid_a.clone()
            },
            &uid_a
        ));
        assert!(!is_write_allowed(
            Moderator,
            &Pref {
                subject_id: uid_b.clone()
            },
            &uid_a
        ));

        // Broadcaster: same as mod
        assert!(is_write_allowed(
            Broadcaster,
            &User {
                subject_id: uid_b.clone()
            },
            &uid_a
        ));
        assert!(is_write_allowed(Broadcaster, &Lore, &uid_a));
        assert!(is_write_allowed(
            Broadcaster,
            &Pref {
                subject_id: uid_a.clone()
            },
            &uid_a
        ));
        assert!(!is_write_allowed(
            Broadcaster,
            &Pref { subject_id: uid_b },
            &uid_a
        ));
    }

    #[test]
    fn trust_level_regular_self_is_self_claim() {
        let uid = "1".to_string();
        assert_eq!(
            trust_level_for(
                UserRole::Regular,
                &Scope::User {
                    subject_id: uid.clone()
                },
                &uid
            ),
            TrustLevel::SelfClaim
        );
    }

    #[test]
    fn trust_level_moderator_other_user_is_mod_broadcaster() {
        let speaker = "1".to_string();
        let other = "2".to_string();
        assert_eq!(
            trust_level_for(
                UserRole::Moderator,
                &Scope::User { subject_id: other },
                &speaker
            ),
            TrustLevel::ModBroadcaster
        );
    }

    #[test]
    fn trust_level_moderator_self_user_is_self_claim() {
        // Self-write invariant: mods don't get the corroboration bonus for
        // claims about themselves.
        let uid = "1".to_string();
        assert_eq!(
            trust_level_for(
                UserRole::Moderator,
                &Scope::User {
                    subject_id: uid.clone()
                },
                &uid
            ),
            TrustLevel::SelfClaim
        );
    }

    #[test]
    fn trust_level_moderator_lore_is_mod_broadcaster() {
        let uid = "1".to_string();
        assert_eq!(
            trust_level_for(UserRole::Moderator, &Scope::Lore, &uid),
            TrustLevel::ModBroadcaster
        );
    }

    #[test]
    fn trust_level_broadcaster_self_pref_is_self_claim() {
        let uid = "1".to_string();
        assert_eq!(
            trust_level_for(
                UserRole::Broadcaster,
                &Scope::Pref {
                    subject_id: uid.clone()
                },
                &uid
            ),
            TrustLevel::SelfClaim
        );
    }

    #[test]
    fn trust_level_regular_lore_is_self_claim() {
        // Regulars can't write Lore at all (rejected by is_write_allowed),
        // but if they did the trust level would fall through to SelfClaim.
        let uid = "1".to_string();
        assert_eq!(
            trust_level_for(UserRole::Regular, &Scope::Lore, &uid),
            TrustLevel::SelfClaim
        );
    }

    #[test]
    fn seed_confidence_values() {
        assert_eq!(seed_confidence(TrustLevel::SelfClaim), 70);
        assert_eq!(seed_confidence(TrustLevel::ModBroadcaster), 90);
        assert_eq!(seed_confidence(TrustLevel::ThirdParty), 30);
    }

    use proptest::prelude::*;

    fn role_strategy() -> impl Strategy<Value = UserRole> {
        prop_oneof![
            Just(UserRole::Regular),
            Just(UserRole::Moderator),
            Just(UserRole::Broadcaster),
        ]
    }

    proptest! {
        /// A regular user can never write User/Pref facts about a subject
        /// whose id differs from their own. Lore is separately gated.
        #[test]
        fn regular_cannot_write_for_other_subject(
            speaker_id in "[0-9]{1,8}",
            subject_id in "[0-9]{1,8}",
            which_scope in 0u32..3,
        ) {
            let scope = match which_scope {
                0 => Scope::User { subject_id: subject_id.clone() },
                1 => Scope::Pref { subject_id: subject_id.clone() },
                _ => Scope::Lore,
            };
            if speaker_id != subject_id && !matches!(scope, Scope::Lore) {
                prop_assert!(!is_write_allowed(UserRole::Regular, &scope, &speaker_id));
            }
        }

        /// Pref scope is self-only for every role — the corroboration
        /// privilege granted to moderators and broadcasters does not extend
        /// to writing preferences on behalf of another user.
        #[test]
        fn pref_always_self_only(
            role in role_strategy(),
            speaker_id in "[0-9]{1,8}",
            subject_id in "[0-9]{1,8}",
        ) {
            let scope = Scope::Pref { subject_id: subject_id.clone() };
            if speaker_id != subject_id {
                prop_assert!(
                    !is_write_allowed(role, &scope, &speaker_id),
                    "pref write by {:?} allowed for {} != {}",
                    role, subject_id, speaker_id,
                );
            }
        }
    }
}
