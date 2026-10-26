# Fuzzing

Targets live in `fuzz/` (cargo-fuzz, libFuzzer). The crate is excluded from
the workspace and needs nightly; CI does not build it.

```bash
cargo install cargo-fuzz
cargo +nightly fuzz list
cargo +nightly fuzz run identifiers -- -max_total_time=60
```

| Target | Input | Checks |
|--------|-------|--------|
| `tei_parse` | TEI XML string | no panic; result serializes |
| `identifiers` | string | DOI/arXiv/ISBN normalizers never panic and are idempotent |
| `index_from_bytes` | bytes | decode rejects or yields an index that validates and searches |
| `chunking` | `(max, text)` | chunks non-empty and within `max` chars |
| `book_toc` | text | TOC/slice never panic; slices within input |
| `selector` | selector string | `selector::evaluate` never panics |

Crashes land in `fuzz/artifacts/<target>/`. Reproduce:

```bash
cargo +nightly fuzz run <target> fuzz/artifacts/<target>/crash-<hash>
```

Turn a fixed crash into a regression test next to the code it exercises
(proptest regressions go in `proptest-regressions/`).
