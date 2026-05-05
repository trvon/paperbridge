use clap::Parser;
use paperseed::app::{CorpusPaths, ImportRequest, IngestRequest, default_corpus_root};
use paperseed::cli::{Cli, Command, CorpusAction, ExportFormat, SeedAction};
use paperseed::sources::metadata_from_paperbridge_json;

fn main() -> paperseed::Result<()> {
    let cli = Cli::parse();
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
        } => handle_corpus(
            CorpusAction::Import {
                path,
                title,
                license,
            },
            &paths,
            json,
        )?,
        Command::Ingest {
            metadata,
            file,
            license,
        } => handle_corpus(
            CorpusAction::Ingest {
                metadata,
                file,
                license,
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
            let db = paperseed::app::status(paths)?;
            if json {
                print_json(&serde_json::json!({
                    "root": paths.root,
                    "papers": db.papers.len(),
                    "policy": "lawful-open-access and user-owned imports only"
                }))?;
            } else {
                println!("paperseed: local corpus ready");
                println!("root: {}", paths.root.display());
                println!("papers: {}", db.papers.len());
                println!("policy: lawful-open-access and user-owned imports only");
            }
        }
        CorpusAction::Import {
            path,
            title,
            license,
        } => {
            let paper = paperseed::app::import(
                paths,
                ImportRequest {
                    path,
                    title,
                    license,
                    yams_hash: None,
                },
            )?;
            if json {
                print_json(&paper)?;
            } else {
                println!("imported: {}", paper.metadata.id);
                println!("title: {}", paper.metadata.title);
                println!("hash: {}", paper.file.hash);
                println!("stored: {}", paper.file.path.display());
            }
        }
        CorpusAction::Ingest {
            metadata,
            file,
            license,
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
