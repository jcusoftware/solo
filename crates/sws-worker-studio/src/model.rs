//! Host-testable enums mirroring the `CHECK` constraints in
//! `migrations/0001_registry.sql`. The string forms are the stored values.

macro_rules! db_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &[$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            /// Exact, case-sensitive parse; unknown input is `None`.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$variant),)+ _ => None }
            }
        }
    };
}

db_enum!(
    /// Workspace membership role. Exactly one `Owner` per workspace.
    Role { Owner => "owner", Publisher => "publisher", Editor => "editor" }
);

db_enum!(
    /// API token scope.
    Scope { Edit => "edit", Publish => "publish", Activate => "activate" }
);

db_enum!(
    /// Where an asset's bytes live (blob storage itself is #63).
    StorageBackend { D1 => "d1", R2 => "r2" }
);

db_enum!(
    /// Deployment lifecycle (the pipeline is #64/#66).
    DeploymentStatus {
        Queued => "queued",
        Building => "building",
        Uploading => "uploading",
        Succeeded => "succeeded",
        Failed => "failed",
        Cancelled => "cancelled",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(*r));
        }
        for s in Scope::ALL {
            assert_eq!(Scope::parse(s.as_str()), Some(*s));
        }
        for b in StorageBackend::ALL {
            assert_eq!(StorageBackend::parse(b.as_str()), Some(*b));
        }
        for d in DeploymentStatus::ALL {
            assert_eq!(DeploymentStatus::parse(d.as_str()), Some(*d));
        }
    }

    #[test]
    fn rejects_unknown_and_wrong_case() {
        assert_eq!(Role::parse("admin"), None);
        assert_eq!(Role::parse("Owner"), None);
        assert_eq!(Scope::parse(""), None);
        assert_eq!(StorageBackend::parse("s3"), None);
    }
}
