//! Authenticated dashboard tier.
//!
//! Ordered so `session.role >= required` is the gate predicate; inserting
//! a new tier later means slotting it in at the right ordinal.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Mod,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Mod => "mod",
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
}
