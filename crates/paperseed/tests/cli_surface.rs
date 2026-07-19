use clap::{CommandFactory, Parser};
use paperseed::cli::Cli;

fn render_help(cmd: &mut clap::Command) -> String {
    let mut buf = Vec::new();
    cmd.write_help(&mut buf).expect("write help");
    String::from_utf8(buf).expect("help is utf8")
}

#[test]
fn top_level_help_advertises_canonical_domains_only() {
    let mut cmd = Cli::command();
    let help = render_help(&mut cmd);

    for command in ["corpus", "seed"] {
        assert!(
            help.contains(command),
            "top-level --help missing command '{command}'\n--- help ---\n{help}"
        );
    }

    for removed_root in ["search", "fetch", "sources", "query", "import", "export"] {
        assert!(
            !help.contains(&format!("  {removed_root}")),
            "root help should not advertise '{removed_root}' after Paperbridge integration refocus\n--- help ---\n{help}"
        );
    }
}

#[test]
fn canonical_commands_parse() {
    let cases: &[&[&str]] = &[
        &["paperseed", "corpus", "status"],
        &["paperseed", "corpus", "list"],
        &["paperseed", "corpus", "show", "abc123"],
        &["paperseed", "corpus", "remove", "abc123"],
        &[
            "paperseed",
            "corpus",
            "import",
            "paper.pdf",
            "--license",
            "cc-by",
            "--no-fulltext",
        ],
        &[
            "paperseed",
            "corpus",
            "ingest",
            "--metadata",
            "paper.json",
            "--file",
            "paper.pdf",
            "--license",
            "cc-by",
            "--no-fulltext",
        ],
        &["paperseed", "corpus", "query", "--q", "induction heads"],
        &["paperseed", "corpus", "export", "--format", "bibtex"],
        &["paperseed", "seed", "check", "--paper-id", "abc123"],
        &["paperseed", "seed", "create", "--paper-id", "abc123"],
    ];

    for argv in cases {
        Cli::try_parse_from(*argv)
            .unwrap_or_else(|e| panic!("command should parse: {argv:?}\n{e}"));
    }
}

#[test]
fn hidden_legacy_corpus_aliases_still_parse_temporarily() {
    let cases: &[&[&str]] = &[
        &["paperseed", "status"],
        &["paperseed", "import", "paper.pdf", "--license", "cc-by"],
        &["paperseed", "query", "--q", "induction heads"],
        &["paperseed", "export", "--format", "bibtex"],
    ];

    for argv in cases {
        Cli::try_parse_from(*argv)
            .unwrap_or_else(|e| panic!("legacy command should parse: {argv:?}\n{e}"));
    }
}
