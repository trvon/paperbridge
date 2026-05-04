use paperseed::models::{CorpusAction, License};
use paperseed::policy::{evaluate, parse_license};

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
