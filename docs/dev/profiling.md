# Profiling and benchmarks

## Stage timings (no rebuild)

```bash
PAPERBRIDGE_TRACE_SPANS=close RUST_LOG=paperbridge=debug,warn \
  paperbridge papers search "attention is all you need" --limit 3
```

`PAPERBRIDGE_TRACE_SPANS`:

- `off` (default) — events only
- `close` — one stderr line per closed span with `time.busy` / `time.idle`
- `json` — same, newline-delimited JSON

`time.busy` is time the span's future spent being polled (CPU or blocking
work on an async worker); `time.idle` is time spent waiting (network,
timers). A large `busy` on an async span means synchronous work that should
move to `spawn_blocking` or be cached.

Spans:

| Span | Level | Covers |
|------|-------|--------|
| `search_papers` | info | whole search request |
| `search_fanout` | info | external source fan-out |
| `run_source{source}` | debug | one external source |
| `paperseed_cache_search` | debug | local BM25F cache query |
| `research_search` | debug | YAMS research workspace query |
| `annotate_cached_hits` | debug | cache flags on hits |
| `open_paper` | info | whole open request |
| `download_open_fulltext` | debug | PDF download |
| `blocking{stage}` | debug | `pdf_extract`, `tei_parse` on the blocking pool |
| `get_paper_structure`, `try_grobid_structure` | debug | structure build |

Each HTTP attempt also logs `http attempt` at debug with `cooldown_ms`,
`queue_ms`, and `ttfb_ms`. Per-component totals and maxima are reported by
`paperbridge status` and the `backend_info` MCP tool under
`request_router.components`.

## Benchmarks

```bash
cargo bench -p paperseed-index      # tokenize, build, search, upsert, codec
cargo bench -p paperseed            # PDF extraction on fixture PDFs
cargo bench -p paperbridge          # chunking, sections, TEI, dedupe, ids
cargo bench -p paperbridge -- dedupe   # filter by name
```

Criterion keeps the previous run under `target/criterion` and prints the
change on the next run. CI only checks that benches compile
(`cargo bench --no-run`).

Crate-private entry points used by benches and fuzz targets live in
`paperbridge::internals` (`#[doc(hidden)]`, not stable API).

## Real-corpus query phases

```bash
cargo run --profile profiling -p paperseed --example profile_query -- \
  ~/.local/share/paperbridge/paperseed "graph neural network" 3
```

Prints corpus load, index decode, search, hit mapping, repeated
`search_scored` (cached index), and a full index rebuild.

## Sampling profilers

```bash
cargo build --profile profiling     # release codegen + line tables, not stripped
samply record target/profiling/paperbridge papers search "..."
# macOS: xcrun xctrace record --template 'Time Profiler' --launch -- target/profiling/paperbridge ...
```

`[profile.release]` strips symbols; use `profiling` for symbolized stacks.
