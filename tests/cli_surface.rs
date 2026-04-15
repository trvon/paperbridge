use clap::{CommandFactory, Parser};
use paperbridge::cli::Cli;

fn render_help(cmd: &mut clap::Command) -> String {
    let mut buf = Vec::new();
    cmd.write_help(&mut buf).expect("write help");
    String::from_utf8(buf).expect("help is utf8")
}

#[test]
fn top_level_help_advertises_canonical_groups() {
    let mut cmd = Cli::command();
    let help = render_help(&mut cmd);

    for canonical in [
        "library",
        "item",
        "collection",
        "papers",
        "config",
        "status",
        "serve",
    ] {
        assert!(
            help.contains(canonical),
            "top-level --help missing canonical group '{canonical}'\n--- help ---\n{help}"
        );
    }
}

#[test]
fn top_level_help_hides_legacy_aliases() {
    let cmd = Cli::command();
    let visible: Vec<&str> = cmd
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.get_name())
        .collect();

    for legacy in [
        "query",
        "collections",
        "read",
        "read-search",
        "create-item",
        "update-item",
        "delete-item",
        "validate-item",
        "create-collection",
        "update-collection",
        "delete-collection",
        "search-papers",
        "resolve-doi",
        "backend-info",
    ] {
        assert!(
            !visible.contains(&legacy),
            "legacy alias '{legacy}' is visible in the top-level command list (should be hidden); visible={visible:?}"
        );
    }
}

#[test]
fn legacy_aliases_still_parse() {
    let cases: &[&[&str]] = &[
        &["paperbridge", "backend-info"],
        &["paperbridge", "query", "--q", "x"],
        &["paperbridge", "collections"],
        &["paperbridge", "read", "--item-key", "K"],
        &["paperbridge", "read-search", "--q", "x"],
        &["paperbridge", "create-item", "--file", "f.json"],
        &["paperbridge", "update-item", "--file", "f.json"],
        &["paperbridge", "delete-item", "--file", "f.json"],
        &["paperbridge", "validate-item", "--file", "f.json"],
        &["paperbridge", "create-collection", "--name", "n"],
        &["paperbridge", "update-collection", "--file", "f.json"],
        &["paperbridge", "delete-collection", "--file", "f.json"],
        &["paperbridge", "search-papers", "--q", "x"],
        &["paperbridge", "resolve-doi", "--doi", "10.1/x"],
    ];

    for argv in cases {
        Cli::try_parse_from(*argv)
            .unwrap_or_else(|e| panic!("legacy alias should still parse: {argv:?}\n{e}"));
    }
}

#[test]
fn canonical_subtree_help_lists_actions() {
    let mut cmd = Cli::command();

    let library_help = render_help(
        cmd.find_subcommand_mut("library")
            .expect("library subtree exists"),
    );
    for action in ["query", "collections", "read", "read-search"] {
        assert!(
            library_help.contains(action),
            "library --help missing action '{action}'\n{library_help}"
        );
    }

    let item_help = render_help(
        cmd.find_subcommand_mut("item")
            .expect("item subtree exists"),
    );
    for action in ["create", "update", "delete", "validate"] {
        assert!(
            item_help.contains(action),
            "item --help missing action '{action}'\n{item_help}"
        );
    }

    let papers_help = render_help(
        cmd.find_subcommand_mut("papers")
            .expect("papers subtree exists"),
    );
    for action in ["search", "resolve-doi"] {
        assert!(
            papers_help.contains(action),
            "papers --help missing action '{action}'\n{papers_help}"
        );
    }
}

#[test]
fn papers_search_sources_validated_at_parse_time() {
    let err = Cli::try_parse_from([
        "paperbridge",
        "papers",
        "search",
        "--q",
        "x",
        "--sources",
        "definitely-not-a-source",
    ])
    .expect_err("unknown source must fail at parse time");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("invalid value") || msg.contains("possible values"),
        "expected parse-time validation error, got: {msg}"
    );
}
