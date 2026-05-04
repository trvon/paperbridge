use crate::models::{CorpusAction, License};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    pub allowed: bool,
    pub reason: &'static str,
}

impl PolicyDecision {
    pub const fn allow(reason: &'static str) -> Self {
        Self {
            allowed: true,
            reason,
        }
    }

    pub const fn block(reason: &'static str) -> Self {
        Self {
            allowed: false,
            reason,
        }
    }
}

pub fn evaluate(action: CorpusAction, license: License) -> PolicyDecision {
    use CorpusAction::*;
    use License::*;

    match action {
        StorePrivate => match license {
            Restricted => PolicyDecision::block(
                "restricted files are not imported without explicit user-owned-private classification",
            ),
            _ => PolicyDecision::allow(
                "private local storage is allowed for user-provided or open material",
            ),
        },
        Download => match license {
            Cc0 | CcBy | CcBySa | PublicDomain | OpenGovernment => {
                PolicyDecision::allow("license permits lawful open download")
            }
            UserOwnedPrivate => PolicyDecision::block(
                "user-owned-private files must be imported locally, not downloaded by the tool",
            ),
            Unknown | Restricted => PolicyDecision::block(
                "download requires known open-access or public-domain license",
            ),
        },
        CacheOpenAccess => match license {
            Restricted => {
                PolicyDecision::block("restricted files cannot be cached from external sources")
            }
            _ => PolicyDecision::allow(
                "open-access resolver location may be cached for private local corpus use; seeding still requires redistribution rights",
            ),
        },
        SeedRedistribute => match license {
            Cc0 | CcBy | CcBySa | PublicDomain | OpenGovernment => {
                PolicyDecision::allow("license permits redistribution/seeding")
            }
            UserOwnedPrivate | Unknown | Restricted => {
                PolicyDecision::block("seeding requires explicit redistribution rights")
            }
        },
    }
}

pub fn parse_license(input: &str) -> License {
    match input.trim().to_ascii_lowercase().as_str() {
        "cc0" | "cc-zero" => License::Cc0,
        "cc-by" | "cc by" => License::CcBy,
        "cc-by-sa" | "cc by sa" => License::CcBySa,
        "public-domain" | "public domain" | "pd" => License::PublicDomain,
        "open-government" | "open government" => License::OpenGovernment,
        "user-owned-private" | "private" | "user-owned" => License::UserOwnedPrivate,
        "restricted" => License::Restricted,
        _ => License::Unknown,
    }
}

pub fn license_slug(license: License) -> &'static str {
    match license {
        License::Cc0 => "cc0",
        License::CcBy => "cc-by",
        License::CcBySa => "cc-by-sa",
        License::PublicDomain => "public-domain",
        License::OpenGovernment => "open-government",
        License::UserOwnedPrivate => "user-owned-private",
        License::Unknown => "unknown",
        License::Restricted => "restricted",
    }
}
