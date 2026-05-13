//! Authenticated dashboard tier.
//!
//! Ordered so `session.role >= required` is the gate predicate; inserting
//! a new tier later means slotting it in at the right ordinal.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Mod,
    Owner,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Mod => "mod",
            Role::Owner => "owner",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Role;

    #[test]
    fn viewer_is_lower_than_mod() {
        assert!(Role::Viewer < Role::Mod);
        assert!(Role::Mod >= Role::Viewer);
        assert!(Role::Viewer >= Role::Viewer);
    }

    #[test]
    fn labels_match_tracing_fields() {
        assert_eq!(Role::Viewer.label(), "viewer");
        assert_eq!(Role::Mod.label(), "mod");
    }

    #[test]
    fn owner_is_above_mod() {
        assert!(Role::Owner > Role::Mod);
        assert!(Role::Owner > Role::Viewer);
    }

    #[test]
    fn owner_label() {
        assert_eq!(Role::Owner.label(), "owner");
    }
}
