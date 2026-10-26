use paperseed::models::{CorpusAction, License};
use paperseed::policy::{evaluate, license_slug, parse_license};

#[test]
fn open_licenses_can_be_downloaded_and_seeded() {
    for license in [
        License::Cc0,
        License::CcBy,
        License::CcBySa,
        License::PublicDomain,
    ] {
        assert!(evaluate(CorpusAction::Download, license).allowed);
        assert!(evaluate(CorpusAction::SeedRedistribute, license).allowed);
    }
}

#[test]
fn unknown_and_private_material_cannot_be_seeded() {
    for license in [
        License::Unknown,
        License::Restricted,
        License::UserOwnedPrivate,
    ] {
        let decision = evaluate(CorpusAction::SeedRedistribute, license);
        assert!(!decision.allowed, "{license:?} should not be seedable");
    }
}

#[test]
fn license_parser_accepts_common_aliases() {
    assert_eq!(parse_license("CC BY"), License::CcBy);
    assert_eq!(parse_license("pd"), License::PublicDomain);
    assert_eq!(parse_license("private"), License::UserOwnedPrivate);
    assert_eq!(parse_license("something else"), License::Unknown);
}

const ALL_LICENSES: [License; 8] = [
    License::Cc0,
    License::CcBy,
    License::CcBySa,
    License::PublicDomain,
    License::OpenGovernment,
    License::UserOwnedPrivate,
    License::Unknown,
    License::Restricted,
];

#[test]
fn license_slug_round_trips_and_matches_serde_name() {
    for license in ALL_LICENSES {
        let slug = license_slug(license);
        assert_eq!(parse_license(slug), license);
        assert_eq!(
            parse_license(&format!("  {} ", slug.to_uppercase())),
            license
        );
        assert_eq!(
            serde_json::to_value(license).unwrap(),
            serde_json::Value::String(slug.to_string())
        );
    }
}

/// Permissions nest: seeding implies download, download implies caching,
/// and caching is allowed exactly when private storage is. User-owned
/// private files may be cached/stored but never downloaded by the tool.
#[test]
fn allowed_actions_form_a_lattice() {
    for license in ALL_LICENSES {
        let allowed = |action| evaluate(action, license).allowed;
        let seed = allowed(CorpusAction::SeedRedistribute);
        let download = allowed(CorpusAction::Download);
        let cache = allowed(CorpusAction::CacheOpenAccess);
        let store = allowed(CorpusAction::StorePrivate);
        assert!(!seed || download, "{license:?}: seed without download");
        assert!(!download || cache, "{license:?}: download without cache");
        assert_eq!(cache, store, "{license:?}: cache/store disagree");
        for action in [
            CorpusAction::SeedRedistribute,
            CorpusAction::Download,
            CorpusAction::CacheOpenAccess,
            CorpusAction::StorePrivate,
        ] {
            assert!(!evaluate(action, license).reason.is_empty());
        }
    }
    assert!(!evaluate(CorpusAction::Download, License::UserOwnedPrivate).allowed);
    assert!(evaluate(CorpusAction::StorePrivate, License::UserOwnedPrivate).allowed);
    assert!(!evaluate(CorpusAction::StorePrivate, License::Restricted).allowed);
}

proptest::proptest! {
    #[test]
    fn parse_license_never_panics_and_unknown_is_default(input in "\\PC{0,24}") {
        let license = parse_license(&input);
        if license != License::Unknown {
            proptest::prop_assert!(!input.trim().is_empty());
        }
    }
}
