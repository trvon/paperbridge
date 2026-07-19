# paperseed

`paperseed` is a Rust, local-first scholarly corpus manager designed to
complement `paperbridge`.

Paperseed is vendored under Paperbridge. Prefer the inherited Paperbridge CLI (`paperbridge paperseed ...`) so corpus operations use Paperbridge config. The standalone `paperseed` binary is a thin development/debug fallback only.

It helps researchers and Paperbridge integrations:

- import files they already have rights to store,
- ingest Paperbridge/Zotero-style metadata,
- store files in a content-addressed local corpus,
- query local full text and metadata,
- export corpus metadata as JSON or BibTeX, and
- run license-gated seeding checks.

## Safety boundary

`paperseed` intentionally does **not** scrape shadow libraries, bypass access
controls, or seed copyrighted papers without redistribution rights. Download and
seeding flows are policy-first:

- private user-owned files may be stored locally;
- open/public-domain files may be downloaded/stored when the license is known;
- only explicit redistribution-compatible licenses may be seeded;
- unknown/restricted/private files are blocked from seeding.

## Prototype commands

```bash
paperbridge paperseed corpus status
paperbridge paperseed corpus list
paperbridge paperseed corpus show <id-or-unique-hash-prefix>
paperbridge paperseed corpus import ./paper.pdf --title "Example Paper" --license user-owned-private
paperbridge paperseed corpus import ./large.pdf --license cc-by --no-fulltext
paperbridge paperseed corpus ingest --metadata ./paperbridge-item.json --file ./oa-paper.pdf --license cc-by
paperbridge paperseed corpus query --q "induction heads"
paperbridge paperseed corpus export --format bibtex
paperbridge paperseed corpus remove <id-or-unique-hash-prefix>
paperbridge paperseed corpus reindex
paperbridge paperseed seed check --paper-id <id-or-hash-prefix>
paperbridge paperseed seed create --paper-id <id-or-hash-prefix>
```

Use `--json` for machine-readable output on supported commands:

```bash
paperseed --json corpus status
paperseed --json corpus query --q "graph learning"
```

Use `--corpus-root` or `PAPERSEED_HOME` to choose where the corpus is stored:

```bash
paperseed --corpus-root /tmp/paperseed-demo corpus import ./paper.txt --license cc-by
```

Default corpus root follows XDG data-home conventions: `$XDG_DATA_HOME/paperbridge/paperseed`, falling back to `~/.local/share/paperbridge/paperseed`. Through Paperbridge config, `paperseed_yams_enabled = true` lets Paperseed experimentally index imports into YAMS when its daemon is running and use YAMS search with local JSON fallback; set it false for local-corpus-only behavior.

Corpus layout:

```text
<corpus-root>/
├── corpus.json
├── corpus.idx.bin
├── files/
│   └── <hash-prefix>/
│       └── <blake3-hash>.<ext>
├── text/
│   └── <hash-prefix>/
│       └── <blake3-hash>.txt
└── seeds/
    └── <paper-id>.json
```

`corpus.json` contains metadata only; extracted text lives in `text/` blobs and
the BM25F postings use the binary `corpus.idx.bin`. `corpus status` reports
paper/index counts and warns on drift. `--no-fulltext` avoids synchronous
extraction and extracts lazily on first read; PDF extraction does not include
OCR.

## Paperbridge integration boundary

Paperbridge integrates Paperseed as a Rust library through its `paperseed_api` module.
Use Paperbridge for external paper discovery and resolver workflows; use Paperseed for
local corpus and seeding operations.

## Paperbridge integration

`ingest` accepts flexible Paperbridge/Zotero-shaped JSON. Example accepted fields:

```json
{
  "data": {
    "title": "Graph Learning at Scale",
    "DOI": "10.5555/graph",
    "date": "2024-08-01",
    "publicationTitle": "Systems Journal",
    "creators": [{ "firstName": "Grace", "lastName": "Hopper" }],
    "url": "https://example.org/graph",
    "rights": "cc-by"
  }
}
```

## Development

```bash
cargo fmt --all
cargo test
```
