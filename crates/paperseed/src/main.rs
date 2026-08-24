use clap::Parser;
use paperseed::app::{CorpusPaths, ImportRequest, IngestRequest, default_corpus_root};
use paperseed::cli::{Cli, Command, CorpusAction, ExportFormat, SeedAction};
use paperseed::sources::metadata_from_paperbridge_json;

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(error) = run(cli) {
        print_runtime_error(&error, json);
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> paperseed::Result<()> {
    let json = cli.json;
    let paths = CorpusPaths::new(cli.corpus_root.unwrap_or_else(default_corpus_root));

    match cli.command.unwrap_or(Command::Corpus {
        action: CorpusAction::Status,
    }) {
        Command::Corpus { action } => handle_corpus(action, &paths, json)?,
        Command::Seed { action } => handle_seed(action, &paths, json)?,

        // Hidden compatibility paths while the prototype settles.
        Command::Status => handle_corpus(CorpusAction::Status, &paths, json)?,
        Command::Import {
            path,
            title,
            license,
            no_fulltext,
        } => handle_corpus(
            CorpusAction::Import {
                path,
                title,
                license,
                no_fulltext,
            },
            &paths,
            json,
        )?,
        Command::Ingest {
            metadata,
            file,
            license,
            no_fulltext,
        } => handle_corpus(
            CorpusAction::Ingest {
                metadata,
                file,
                license,
                no_fulltext,
            },
            &paths,
            json,
        )?,
        Command::Query { q } => handle_corpus(CorpusAction::Query { q }, &paths, json)?,
        Command::Export { format } => handle_corpus(CorpusAction::Export { format }, &paths, json)?,
    }

    Ok(())
}

fn handle_corpus(action: CorpusAction, paths: &CorpusPaths, json: bool) -> paperseed::Result<()> {
    match action {
        CorpusAction::Status => {
            let status = paperseed::app::status_summary(paths)?;
            if json {
                print_json(&serde_json::json!({
                    "root": status.root,
                    "papers": status.papers,
                    "index_docs": status.index_docs,
                    "index_in_sync": status.index_in_sync,
                    "policy": "lawful-open-access and user-owned imports only"
                }))?;
            } else {
                println!("paperseed: local corpus ready");
                println!("root: {}", paths.root.display());
                println!("papers: {}", status.papers);
                println!(
                    "index docs: {}",
                    status
                        .index_docs
                        .map_or_else(|| "missing".to_string(), |count| count.to_string())
                );
                if !status.index_in_sync {
                    println!("warning: search index is stale; run `paperseed corpus reindex`");
                }
                println!("policy: lawful-open-access and user-owned imports only");
            }
        }
        CorpusAction::List => {
            let entries = paperseed::app::list_entries(paths)?;
            if json {
                print_json(&entries)?;
            } else if entries.is_empty() {
                println!("local corpus is empty");
            } else {
                for entry in entries {
                    println!(
                        "{}\t{}\t{}",
                        entry.paper.metadata.id,
                        entry.paper.metadata.title,
                        entry.paper.file.path.display()
                    );
                }
            }
        }
        CorpusAction::Show { id } => {
            let entry = paperseed::app::get_entry(paths, &id)?;
            if json {
                print_json(&entry)?;
            } else {
                println!("id: {}", entry.paper.metadata.id);
                println!("title: {}", entry.paper.metadata.title);
                println!("hash: {}", entry.paper.file.hash);
                println!("stored: {}", entry.paper.file.path.display());
            }
        }
        CorpusAction::Remove { id } => {
            let entry = paperseed::app::remove_entry(paths, &id)?;
            if json {
                print_json(&serde_json::json!({
                    "removed": entry.paper.metadata.id,
                    "title": entry.paper.metadata.title,
                    "stored_path": entry.paper.file.path,
                }))?;
            } else {
                println!("removed: {}", entry.paper.metadata.id);
                println!("title: {}", entry.paper.metadata.title);
                println!("stored file deleted: {}", entry.paper.file.path.display());
            }
        }
        CorpusAction::Import {
            path,
            title,
            license,
            no_fulltext,
        } => {
            let paper = paperseed::app::import(
                paths,
                ImportRequest {
                    path,
                    title,
                    license,
                    yams_hash: None,
                    extract_full_text: !no_fulltext,
                },
            )?;
            if json {
                print_json(&paper)?;
            } else {
                println!("imported: {}", paper.metadata.id);
                println!("title: {}", paper.metadata.title);
                println!("hash: {}", paper.file.hash);
                println!("stored: {}", paper.file.path.display());
                if no_fulltext {
                    println!("full text: deferred; later PDF extraction does not perform OCR");
                }
            }
        }
        CorpusAction::Ingest {
            metadata,
            file,
            license,
            no_fulltext,
        } => {
            let raw = std::fs::read_to_string(metadata)?;
            let metadata = metadata_from_paperbridge_json(&raw)?;
            let paper = paperseed::app::ingest(
                paths,
                IngestRequest {
                    path: file,
                    metadata,
                    license,
                    yams_hash: None,
                    extract_full_text: !no_fulltext,
                },
            )?;
            if json {
                print_json(&paper)?;
            } else {
                println!("ingested: {}", paper.metadata.id);
                println!("title: {}", paper.metadata.title);
                if let Some(doi) = &paper.metadata.doi {
                    println!("doi: {doi}");
                }
                println!("stored: {}", paper.file.path.display());
                if no_fulltext {
                    println!("full text: deferred; later PDF extraction does not perform OCR");
                }
            }
        }
        CorpusAction::Query { q } => {
            let hits = paperseed::app::query(paths, &q)?;
            if json {
                print_json(&hits)?;
            } else {
                for hit in &hits {
                    println!(
                        "{}\tscore={}\t{}\t{}",
                        hit.id,
                        hit.score,
                        hit.title,
                        hit.path.display()
                    );
                }
                if hits.is_empty() {
                    println!("no local matches");
                }
            }
        }
        CorpusAction::Export { format } => {
            let db = paperseed::app::status(paths)?;
            match format {
                ExportFormat::Json => print_json(&db)?,
                ExportFormat::Bibtex => println!("{}", paperseed::app::export_bibtex(&db)),
            }
        }
        CorpusAction::Reindex => {
            let count = paperseed::app::reindex(paths)?;
            if json {
                print_json(&serde_json::json!({
                    "indexed": count,
                    "index_path": paths.index_path.display().to_string(),
                }))?;
            } else {
                println!("reindexed {count} papers -> {}", paths.index_path.display());
            }
        }
    }
    Ok(())
}

fn handle_seed(action: SeedAction, paths: &CorpusPaths, json: bool) -> paperseed::Result<()> {
    match action {
        SeedAction::Check { paper_id } => {
            let reason = paperseed::app::seed_check(paths, &paper_id)?;
            if json {
                print_json(&serde_json::json!({
                    "paper_id": paper_id,
                    "allowed": true,
                    "reason": reason,
                }))?;
            } else {
                println!("seed allowed: {paper_id}");
                println!("reason: {reason}");
            }
        }
        SeedAction::Create { paper_id } => {
            let manifest = paperseed::app::create_seed_manifest(paths, &paper_id)?;
            if json {
                print_json(&manifest)?;
            } else {
                println!("seed manifest created: {}", manifest.paper_id);
                println!("reason: {}", manifest.reason);
                println!("hash: {}", manifest.hash);
                println!(
                    "manifest: {}",
                    paths
                        .seeds_dir
                        .join(format!("{}.json", manifest.paper_id))
                        .display()
                );
            }
        }
    }
    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> paperseed::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_runtime_error(error: &paperseed::PaperseedError, json: bool) {
    let recovery = recovery_commands(error);
    if json {
        let envelope = serde_json::json!({
            "error": "paperseed operation failed",
            "reason": error.to_string(),
            "try": recovery,
        });
        eprintln!("{envelope}");
    } else {
        eprintln!("paperseed operation failed");
        eprintln!("reason: {error}");
        eprintln!("Try:");
        for command in recovery {
            eprintln!("  {command}");
        }
    }
}

fn recovery_commands(error: &paperseed::PaperseedError) -> Vec<&'static str> {
    match error {
        paperseed::PaperseedError::PaperNotFound(_)
        | paperseed::PaperseedError::EmptyPaperId
        | paperseed::PaperseedError::AmbiguousPaperId { .. } => {
            vec!["paperseed corpus list", "paperseed corpus show <exact-id>"]
        }
        paperseed::PaperseedError::CorruptCorpus { .. } => vec![
            "inspect the corpus.json.bad.* quarantine file",
            "restore a known-good corpus.json backup",
        ],
        paperseed::PaperseedError::MissingResolverEmail => vec![
            "configure a real Unpaywall contact email in Paperbridge",
            "use the OpenAlex resolver",
        ],
        paperseed::PaperseedError::PolicyBlocked { .. }
        | paperseed::PaperseedError::IntegrityMismatch { .. } => vec![
            "paperseed seed check --paper-id <id>",
            "paperseed corpus show <id>",
        ],
        _ => vec!["paperseed corpus status", "paperseed --help"],
    }
}
