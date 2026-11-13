//! Timing harnesses for `papers fetch`. Every test is ignored: they need a
//! local Chrome and print measurements instead of asserting behaviour.
//!
//! `cargo test --release --test fetch_perf -- --ignored --nocapture --test-threads=1`
//!
//! Chrome runs headless on an ephemeral port with a temporary profile.
//! `PAPERBRIDGE_PERF_MB` (default `2,10,50`) picks the PDF sizes.

use base64::Engine;
use paperbridge::fetch::cdp::{BrowserTiming, CdpTab, CdpTransport, WsCdpTransport};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MIB: usize = 1024 * 1024;

fn sizes_mb() -> Vec<usize> {
    std::env::var("PAPERBRIDGE_PERF_MB")
        .unwrap_or_else(|_| "2,10,50".to_string())
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect()
}

/// A PDF-shaped body of incompressible bytes.
fn synthetic_pdf(len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len);
    bytes.extend_from_slice(b"%PDF-1.7\n");
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    while bytes.len() < len - 6 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.push((state >> 24) as u8);
    }
    bytes.truncate(len - 6);
    bytes.extend_from_slice(b"\n%%EOF");
    bytes
}

fn status_kib(pid: u32, key: &str) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn descendants(root: u32) -> Vec<u32> {
    let mut parents = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            parents.push((pid, status_kib(pid, "PPid:") as u32));
        }
    }
    let mut tree = vec![root];
    let mut index = 0;
    while index < tree.len() {
        let parent = tree[index];
        tree.extend(
            parents
                .iter()
                .filter(|(_, p)| *p == parent)
                .map(|(pid, _)| *pid),
        );
        index += 1;
    }
    tree
}

/// Samples the summed RSS of a process tree and keeps the peak.
struct TreePeak {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    base: u64,
}

impl TreePeak {
    fn start(root: u32) -> Self {
        let sum = move || -> u64 {
            descendants(root)
                .into_iter()
                .map(|pid| status_kib(pid, "VmRSS:"))
                .sum()
        };
        let base = sum();
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(base));
        let (stop_flag, peak_cell) = (stop.clone(), peak.clone());
        std::thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                peak_cell.fetch_max(sum(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        Self { stop, peak, base }
    }

    /// Peak growth over the starting RSS, in MiB.
    fn finish(self) -> f64 {
        self.stop.store(true, Ordering::Relaxed);
        self.peak.load(Ordering::Relaxed).saturating_sub(self.base) as f64 / 1024.0
    }
}

/// Peak RSS growth of this process across `f`, in MiB.
fn reset_own_peak() -> u64 {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    status_kib(std::process::id(), "VmRSS:")
}

fn own_peak_since(base: u64) -> f64 {
    status_kib(std::process::id(), "VmHWM:").saturating_sub(base) as f64 / 1024.0
}

struct Chrome {
    child: tokio::process::Child,
    port: u16,
    _dir: tempfile::TempDir,
}

async fn start_chrome() -> Chrome {
    let chrome = std::env::var("PAPERBRIDGE_TEST_CHROME")
        .unwrap_or_else(|_| "google-chrome-stable".to_string());
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let child = tokio::process::Command::new(chrome)
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--disable-gpu",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start Chrome");
    let port = wait_for_port(&profile).await;
    Chrome {
        child,
        port,
        _dir: dir,
    }
}

async fn wait_for_port(profile: &Path) -> u16 {
    let port_file = profile.join("DevToolsActivePort");
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(&port_file)
            && let Some(line) = text.lines().next()
            && let Ok(parsed) = line.trim().parse::<u16>()
        {
            return parsed;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("Chrome did not write DevToolsActivePort");
}

async fn serve(sizes: &[usize]) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/landing"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<html><body>landing</body></html>", "text/html"),
        )
        .mount(&server)
        .await;
    for mb in sizes {
        Mock::given(method("GET"))
            .and(path(format!("/pdf/{mb}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(synthetic_pdf(mb * MIB), "application/pdf"),
            )
            .mount(&server)
            .await;
    }
    server
}

fn default_timing() -> BrowserTiming {
    BrowserTiming::default()
}

/// The shipped path: `CdpTab::fetch` from a settled landing page.
#[tokio::test]
#[ignore = "timing harness; needs a local Chrome"]
async fn perf_pdf_transfer_shipped_path() {
    let sizes = sizes_mb();
    let server = serve(&sizes).await;
    let mut chrome = start_chrome().await;
    let chrome_pid = chrome.child.id().unwrap();
    let mut transport = WsCdpTransport::connect(&format!("http://127.0.0.1:{}", chrome.port))
        .await
        .unwrap();
    println!("shipped CdpTab::fetch (same-origin fetch from a landing page)");
    println!("size_mb  open_ms  fetch_ms  MiB/s  own_peak_mib  chrome_peak_mib");
    for mb in &sizes {
        for _ in 0..3 {
            let started = Instant::now();
            let (mut tab, _) = CdpTab::open(
                &mut transport,
                &format!("{}/landing", server.uri()),
                default_timing(),
            )
            .await
            .unwrap();
            let open_ms = started.elapsed().as_secs_f64() * 1e3;
            let tree = TreePeak::start(chrome_pid);
            let base = reset_own_peak();
            let started = Instant::now();
            let body = tab
                .fetch(Some(&format!("{}/pdf/{mb}", server.uri())), 100 * MIB)
                .await
                .unwrap();
            let secs = started.elapsed().as_secs_f64();
            let own = own_peak_since(base);
            let chrome_peak = tree.finish();
            assert_eq!(body.bytes.len(), mb * MIB);
            assert_eq!(body.bytes, synthetic_pdf(mb * MIB));
            drop(body);
            tab.close().await.unwrap();
            println!(
                "{mb:7}  {open_ms:7.0}  {:8.0}  {:5.0}  {own:12.0}  {chrome_peak:15.0}",
                secs * 1e3,
                *mb as f64 / secs
            );
        }
    }
    let _ = chrome.child.kill().await;
}

async fn eval(
    transport: &mut WsCdpTransport,
    session: &str,
    expression: &str,
    by_value: bool,
) -> Value {
    let result = transport
        .call(
            Some(session),
            "Runtime.evaluate",
            json!({"expression": expression, "awaitPromise": true, "returnByValue": by_value}),
        )
        .await
        .unwrap();
    assert!(result.get("exceptionDetails").is_none(), "{result}");
    result["result"].clone()
}

/// Candidate transfer paths, driven with raw CDP calls so chunk sizes and
/// mechanisms can be compared without changing the shipped driver.
#[tokio::test]
#[ignore = "timing harness; needs a local Chrome"]
async fn perf_pdf_transfer_candidates() {
    let sizes = sizes_mb();
    let server = serve(&sizes).await;
    let mut chrome = start_chrome().await;
    let chrome_pid = chrome.child.id().unwrap();
    let mut transport = WsCdpTransport::connect(&format!("http://127.0.0.1:{}", chrome.port))
        .await
        .unwrap();
    let decode = |text: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .unwrap()
    };
    let variants: Vec<(&str, usize)> = vec![
        ("evaluate+btoa", 512 * 1024),
        ("evaluate+btoa", 2 * MIB),
        ("evaluate+btoa", 8 * MIB),
        ("IO.read blob", 512 * 1024),
        ("IO.read blob", 2 * MIB),
        ("IO.read blob", 8 * MIB),
    ];
    println!(
        "variant        chunk_kib  size_mb  fetch_ms  drain_ms  MiB/s  own_peak_mib  chrome_peak_mib"
    );
    for mb in &sizes {
        for (variant, chunk) in &variants {
            let mut best: Option<(f64, f64, f64, f64)> = None;
            for _ in 0..3 {
                let created = transport
                    .call(
                        None,
                        "Target.createTarget",
                        json!({"url": format!("{}/landing", server.uri())}),
                    )
                    .await
                    .unwrap();
                let target = created["targetId"].as_str().unwrap().to_string();
                let attached = transport
                    .call(
                        None,
                        "Target.attachToTarget",
                        json!({"targetId": target, "flatten": true}),
                    )
                    .await
                    .unwrap();
                let session = attached["sessionId"].as_str().unwrap().to_string();
                for _ in 0..200 {
                    let ready = eval(
                        &mut transport,
                        &session,
                        "location.href + document.readyState",
                        true,
                    )
                    .await;
                    if ready["value"]
                        .as_str()
                        .is_some_and(|v| v.contains("/landing") && v.ends_with("complete"))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                let tree = TreePeak::start(chrome_pid);
                let base = reset_own_peak();
                let started = Instant::now();
                let url = format!("{}/pdf/{mb}", server.uri());
                let mut bytes: Vec<u8> = Vec::new();
                let fetch_ms;
                if *variant == "evaluate+btoa" {
                    let length = eval(
                        &mut transport,
                        &session,
                        &format!(
                            "(async () => {{ const r = await fetch('{url}'); globalThis.__b = new Uint8Array(await r.arrayBuffer()); return globalThis.__b.length; }})()"
                        ),
                        true,
                    )
                    .await["value"]
                        .as_u64()
                        .unwrap() as usize;
                    fetch_ms = started.elapsed().as_secs_f64() * 1e3;
                    bytes.reserve(length);
                    let mut start = 0;
                    while start < length {
                        let end = (start + chunk).min(length);
                        let value = eval(
                            &mut transport,
                            &session,
                            &format!(
                                "(() => {{ const s = globalThis.__b.subarray({start}, {end}); let b = ''; for (let i = 0; i < s.length; i += 0x8000) b += String.fromCharCode.apply(null, s.subarray(i, i + 0x8000)); return btoa(b); }})()"
                            ),
                            true,
                        )
                        .await;
                        bytes.extend_from_slice(&decode(value["value"].as_str().unwrap()));
                        start = end;
                    }
                } else {
                    let blob = eval(
                        &mut transport,
                        &session,
                        &format!(
                            "(async () => {{ const r = await fetch('{url}'); globalThis.__b = await r.blob(); return globalThis.__b; }})()"
                        ),
                        false,
                    )
                    .await;
                    fetch_ms = started.elapsed().as_secs_f64() * 1e3;
                    let object_id = blob["objectId"].as_str().unwrap();
                    let resolved = transport
                        .call(
                            Some(&session),
                            "IO.resolveBlob",
                            json!({"objectId": object_id}),
                        )
                        .await
                        .unwrap();
                    let handle = format!("blob:{}", resolved["uuid"].as_str().unwrap());
                    loop {
                        let read = transport
                            .call(
                                Some(&session),
                                "IO.read",
                                json!({"handle": handle, "size": chunk}),
                            )
                            .await
                            .unwrap();
                        let data = read["data"].as_str().unwrap_or_default();
                        if read["base64Encoded"].as_bool().unwrap_or(false) {
                            bytes.extend_from_slice(&decode(data));
                        } else {
                            bytes.extend_from_slice(data.as_bytes());
                        }
                        if read["eof"].as_bool().unwrap_or(false) {
                            break;
                        }
                    }
                    let _ = transport
                        .call(Some(&session), "IO.close", json!({"handle": handle}))
                        .await;
                }
                let total_ms = started.elapsed().as_secs_f64() * 1e3;
                let own = own_peak_since(base);
                let chrome_peak = tree.finish();
                assert_eq!(bytes.len(), mb * MIB, "{variant} {chunk}");
                assert!(
                    bytes == synthetic_pdf(mb * MIB),
                    "{variant} {chunk}: bytes differ"
                );
                drop(bytes);
                transport
                    .call(None, "Target.closeTarget", json!({"targetId": target}))
                    .await
                    .unwrap();
                if best.is_none_or(|(_, total, _, _)| total_ms < total) {
                    best = Some((fetch_ms, total_ms, own, chrome_peak));
                }
            }
            let (fetch_ms, total_ms, own, chrome_peak) = best.unwrap();
            println!(
                "{variant:14} {:9}  {mb:7}  {fetch_ms:8.0}  {:8.0}  {:5.0}  {own:12.0}  {chrome_peak:15.0}",
                chunk / 1024,
                total_ms - fetch_ms,
                *mb as f64 / (total_ms / 1e3)
            );
        }
    }
    let _ = chrome.child.kill().await;
}

/// Cost of the extraction comparison on real PDFs: the built-in extractor
/// against `pdftotext`, and what the shipped rule decides for each file.
/// `PAPERBRIDGE_PERF_PDF_DIR` names a directory searched recursively
/// (default: the paperseed fixtures). Files are only read.
#[tokio::test]
#[ignore = "timing harness; needs pdftotext"]
async fn perf_extraction_comparison() {
    use paperbridge::fetch::extract::{Extractor, run_pdftotext, select_extractor, text_chars};
    let dir = std::env::var("PAPERBRIDGE_PERF_PDF_DIR").unwrap_or_else(|_| {
        format!(
            "{}/crates/paperseed/tests/fixtures",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    let mut files = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(dir)];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "pdf") {
                files.push(path);
            }
        }
    }
    files.sort();
    let verbose = std::env::var("PAPERBRIDGE_PERF_VERBOSE").is_ok();
    let (mut builtin_total, mut pdftotext_total) = (0.0, 0.0);
    let mut rows: Vec<(f64, f64, f64, usize, usize, usize, Extractor)> = Vec::new();
    for file in &files {
        let Ok(bytes) = std::fs::read(file) else {
            continue;
        };
        let started = Instant::now();
        let owned = bytes.clone();
        let builtin = tokio::task::spawn_blocking(move || {
            paperseed::app::extract_pdf_text_from_bytes(&owned)
                .ok()
                .flatten()
        })
        .await
        .unwrap();
        let builtin_ms = started.elapsed().as_secs_f64() * 1e3;
        let builtin_chars = builtin.as_deref().map_or(0, text_chars);
        let started = Instant::now();
        let output = run_pdftotext("pdftotext", file).await;
        let pdftotext_ms = started.elapsed().as_secs_f64() * 1e3;
        let measured = output.as_ref().map(|o| (text_chars(&o.text), o.pages));
        let (extractor, _) = select_extractor(builtin_chars, measured);
        let (chars, pages) = measured.unwrap_or((0, 0));
        builtin_total += builtin_ms;
        pdftotext_total += pdftotext_ms;
        if verbose {
            println!(
                "{:6.1} MiB  builtin {builtin_ms:7.0} ms {builtin_chars:8} ch  pdftotext {pdftotext_ms:6.0} ms {chars:8} ch {pages:4} pp  {extractor:?}",
                bytes.len() as f64 / MIB as f64
            );
        }
        rows.push((
            bytes.len() as f64 / MIB as f64,
            builtin_ms,
            pdftotext_ms,
            builtin_chars,
            chars,
            pages,
            extractor,
        ));
    }
    let n = rows.len().max(1);
    let pct = |mut v: Vec<f64>, q: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * q) as usize]
    };
    let builtin_ms: Vec<f64> = rows.iter().map(|r| r.1).collect();
    let pdftotext_ms: Vec<f64> = rows.iter().map(|r| r.2).collect();
    println!("files {n}");
    println!(
        "builtin   ms: total {builtin_total:.0}  p50 {:.0}  p90 {:.0}  max {:.0}",
        pct(builtin_ms.clone(), 0.5),
        pct(builtin_ms.clone(), 0.9),
        pct(builtin_ms, 1.0)
    );
    println!(
        "pdftotext ms: total {pdftotext_total:.0}  p50 {:.0}  p90 {:.0}  max {:.0}",
        pct(pdftotext_ms.clone(), 0.5),
        pct(pdftotext_ms.clone(), 0.9),
        pct(pdftotext_ms, 1.0)
    );
    let replaced = rows.iter().filter(|r| r.6 == Extractor::Pdftotext).count();
    let none = rows.iter().filter(|r| r.6 == Extractor::None).count();
    println!("shipped rule: pdftotext replaces built-in on {replaced} files, no text on {none}");
    // Candidate rule: run pdftotext only when the built-in text is thin.
    // Pages are unknown without pdftotext, so the gate is characters per MiB
    // and an absolute floor; report how many fallbacks each gate would miss.
    for floor in [2_000usize, 5_000, 10_000, 20_000, 40_000] {
        let skipped: Vec<_> = rows.iter().filter(|r| r.3 >= floor).collect();
        let missed = skipped
            .iter()
            .filter(|r| r.6 == Extractor::Pdftotext)
            .count();
        let saved: f64 = skipped.iter().map(|r| r.2).sum();
        println!(
            "gate builtin_chars >= {floor:6}: skips pdftotext on {} files, saves {saved:.0} ms, misses {missed} of {replaced} fallbacks",
            skipped.len()
        );
    }
}

fn synthetic_entry(index: usize, text_dir: &Path) -> paperseed::db::IndexedPaper {
    use paperseed::models::{License, LocalPaper, PaperMetadata, StoredFile};
    let hash = format!("{index:064x}");
    paperseed::db::IndexedPaper {
        paper: LocalPaper {
            metadata: PaperMetadata {
                id: format!("s{index:011x}"),
                title: format!("Synthetic corpus entry number {index} on measured systems"),
                doi: Some(format!("10.5555/synthetic.{index}")),
                arxiv_id: None,
                isbn: None,
                work_type: Some("paper".to_string()),
                authors: vec!["Ada Lovelace".to_string(), "Alan Turing".to_string()],
                year: Some(2021),
                venue: Some("Proceedings of the Fixture Conference".to_string()),
                abstract_note: Some("An abstract of ordinary length. ".repeat(20)),
                license: License::UserOwnedPrivate,
                source_url: Some(format!("https://doi.org/10.5555/synthetic.{index}")),
            },
            file: StoredFile {
                hash: hash.clone(),
                path: std::path::PathBuf::from(format!("/corpus/files/{hash}.pdf")),
                size_bytes: 1_000_000,
                mime: "application/pdf".to_string(),
            },
        },
        full_text: None,
        full_text_path: Some(text_dir.join(format!("{hash}.txt"))),
        yams_hash: None,
    }
}

fn median_ms(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

/// Cost of the cached-DOI check and of ingest as the corpus grows.
/// `PAPERBRIDGE_PERF_CORPUS` (default `94,10000`) picks the corpus sizes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "timing harness"]
async fn perf_corpus_scaling() {
    use paperbridge::fetch::ingest_pdf;
    use paperbridge::models::CrossrefWork;
    use paperbridge::paperseed_api::PaperseedApi;
    let sizes: Vec<usize> = std::env::var("PAPERBRIDGE_PERF_CORPUS")
        .unwrap_or_else(|_| "94,10000".to_string())
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect();
    let fixture = format!(
        "{}/crates/paperseed/tests/fixtures/arxiv_1408_5939_planar_subgraphs.pdf",
        env!("CARGO_MANIFEST_DIR")
    );
    println!("entries  db_kib  cached_miss_ms  cached_hit_ms  batch100_miss_ms  ingest_ms");
    for entries in sizes {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("corpus");
        let api = PaperseedApi::with_yams(&root, None, paperseed::yams::YamsConfig::disabled());
        let text_dir = api.paths().text_dir.clone();
        std::fs::create_dir_all(&text_dir).unwrap();
        let db = paperseed::db::CorpusDb {
            papers: (0..entries)
                .map(|index| synthetic_entry(index, &text_dir))
                .collect(),
        };
        for entry in &db.papers {
            let body = format!("{} measured systems text. ", entry.paper.metadata.title).repeat(40);
            std::fs::write(entry.full_text_path.as_ref().unwrap(), body).unwrap();
        }
        db.save(&api.paths().db_path).unwrap();
        let db_kib = std::fs::metadata(&api.paths().db_path).unwrap().len() / 1024;
        let time = |doi: String| {
            median_ms(
                (0..15)
                    .map(|_| {
                        let started = Instant::now();
                        std::hint::black_box(api.find_cached_identity(
                            Some(&doi),
                            None,
                            None,
                            None,
                        ));
                        started.elapsed().as_secs_f64() * 1e3
                    })
                    .collect(),
            )
        };
        let miss = time("10.1145/3460120.3484766".to_string());
        let hit = time(format!("10.5555/synthetic.{}", entries / 2));
        let started = Instant::now();
        for index in 0..100 {
            std::hint::black_box(api.find_cached_identity(
                Some(&format!("10.9999/absent.{index}")),
                None,
                None,
                None,
            ));
        }
        let batch = started.elapsed().as_secs_f64() * 1e3;
        let work = CrossrefWork {
            doi: "10.1145/3460120.3484766".to_string(),
            title: Some("Planar subgraphs fixture".to_string()),
            authors: vec!["Ada Lovelace".to_string()],
            year: Some("2021".to_string()),
            journal: None,
            abstract_note: None,
            url: None,
            resource_url: None,
            publisher: None,
            item_type: None,
            oa_pdf_url: None,
        };
        let started = Instant::now();
        let report = ingest_pdf(&api, &work, Path::new(&fixture), "pdftotext")
            .await
            .unwrap();
        let ingest = started.elapsed().as_secs_f64() * 1e3;
        std::hint::black_box(report);
        println!("{entries:7}  {db_kib:6}  {miss:14.2}  {hit:13.2}  {batch:16.0}  {ingest:9.0}");
    }
}

/// Fixed waits on the critical path: time from `CdpTab::open` to a settled
/// page with the default timing, for an HTML landing page and for a route
/// that is the PDF itself, and the whole direct-PDF fetch.
#[tokio::test]
#[ignore = "timing harness; needs a local Chrome"]
async fn perf_navigation_settle() {
    let server = serve(&[2]).await;
    let mut chrome = start_chrome().await;
    let mut transport = WsCdpTransport::connect(&format!("http://127.0.0.1:{}", chrome.port))
        .await
        .unwrap();
    println!("page          open_ms  fetch_ms  total_ms");
    for (label, route) in [("html landing", "/landing"), ("direct pdf", "/pdf/2")] {
        let mut rows = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            let (mut tab, state) = CdpTab::open(
                &mut transport,
                &format!("{}{route}", server.uri()),
                default_timing(),
            )
            .await
            .unwrap();
            let open_ms = started.elapsed().as_secs_f64() * 1e3;
            let is_pdf = state.content_type == "application/pdf";
            assert_eq!(is_pdf, route != "/landing", "{state:?}");
            let url = format!("{}/pdf/2", server.uri());
            let body = tab
                .fetch(if is_pdf { None } else { Some(&url) }, 100 * MIB)
                .await
                .unwrap();
            assert_eq!(body.bytes.len(), 2 * MIB);
            let total_ms = started.elapsed().as_secs_f64() * 1e3;
            tab.close().await.unwrap();
            rows.push((open_ms, total_ms));
        }
        let open = median_ms(rows.iter().map(|r| r.0).collect());
        let total = median_ms(rows.iter().map(|r| r.1).collect());
        println!("{label:12}  {open:7.0}  {:8.0}  {total:8.0}", total - open);
    }
    let _ = chrome.child.kill().await;
}

/// Wall time of `ingest_pdf` (built-in extraction, `pdftotext`, selection,
/// dedupe) over real PDFs into a fresh corpus. `PAPERBRIDGE_PERF_PDF_DIR`
/// as in `perf_extraction_comparison`; at most `PAPERBRIDGE_PERF_INGEST_MAX`
/// files (default 60).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "timing harness; needs pdftotext"]
async fn perf_ingest_wall_time() {
    use paperbridge::fetch::extract::has_pdf_magic;
    use paperbridge::fetch::ingest_pdf;
    use paperbridge::models::CrossrefWork;
    use paperbridge::paperseed_api::PaperseedApi;
    let dir = std::env::var("PAPERBRIDGE_PERF_PDF_DIR").unwrap_or_else(|_| {
        format!(
            "{}/crates/paperseed/tests/fixtures",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    let limit: usize = std::env::var("PAPERBRIDGE_PERF_INGEST_MAX")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(60);
    let mut files = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(dir)];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "pdf")
                && std::fs::read(&path).is_ok_and(|bytes| has_pdf_magic(&bytes))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files.truncate(limit);
    let corpus = tempfile::tempdir().unwrap();
    let api = PaperseedApi::with_yams(
        corpus.path().join("corpus"),
        None,
        paperseed::yams::YamsConfig::disabled(),
    );
    let mut samples = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let work = CrossrefWork {
            doi: format!("10.5555/perf.{index}"),
            title: Some(format!("Perf ingest paper {index}")),
            authors: Vec::new(),
            year: None,
            journal: None,
            abstract_note: None,
            url: None,
            resource_url: None,
            publisher: None,
            item_type: None,
            oa_pdf_url: None,
        };
        let started = Instant::now();
        if ingest_pdf(&api, &work, file, "pdftotext").await.is_ok() {
            samples.push(started.elapsed().as_secs_f64() * 1e3);
        }
    }
    let total: f64 = samples.iter().sum();
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    println!(
        "ingest_pdf over {} files: total {total:.0} ms  p50 {:.0}  p90 {:.0}  max {:.0}",
        sorted.len(),
        sorted[sorted.len() / 2],
        sorted[sorted.len() * 9 / 10],
        sorted[sorted.len() - 1]
    );
}
