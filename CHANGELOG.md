# Changelog

## Unreleased

### Features

* **agent interface:** compact paginated search envelopes with `hit_id`, `match`, `access`, `next`, and multi-source `diagnostics`
* **agent interface:** `open_paper` MCP tool + `papers open` CLI for discover→read by hit_id/DOI/arXiv/item/paper id
* **research:** first-class YAMS discovery with grouped paper projects, stable `research:` IDs, availability state, and off-disk TeX opening
* **paperseed:** synchronous verified YAMS indexing with persisted content hashes for imports and OA mirrors
* **paperseed:** corpus list/show/remove operations, deferred `--no-fulltext` imports, and index-drift status
* **paperseed:** content-addressed text blobs, incremental BM25F updates, and binary index persistence
* **search:** arXiv title/id query adapters; Crossref bibliographic query for multi-word titles
* **search:** DOI-first resolution, conversational GNN query expansion, and query-coverage ranking
* **search:** fixed candidate windows prevent pagination-induced reranking; URL-only/PMID hits prefer executable `url:` IDs when available
* **MCP:** typed output schemas, structured content, bounded recovery errors, accurate tool annotations/server identity, optional `serve --profile core`
* **open:** fresh DOI/arXiv/URL hits can produce fulltext or structure without enabling Paperseed
* **skill:** `prepare_paper_for_skill` / `papers skill` scaffold from paper structure
* **design:** `docs/design/llm-interface.md` contract + task backlog

### Breaking changes (agent/CLI consumers)

* Library `total_count` is nullable with `count_kind`; paper-search totals describe a fixed candidate window, not the entire index. Increase `--per-source` and restart instead of relying on offset to expand discovery.
* `open_paper want=structure` defaults to a bounded outline. Select fields for full content; follow `structure_page` / `chunks_page` continuation and completeness. Metadata reports retrieval status and truncation.
* MCP `query_paper` returns `{value}` instead of a bare selected JSON value; CLI selection output is unchanged.
* Key reads require identity matches. PMID-only opens and missing cache IDs fail explicitly, rather than returning misleading placeholders. Structure provenance distinguishes cached, research, Zotero and direct content.
* MCP runtime execution errors use `isError` with structured recovery; validation errors retain invalid-params. Shared CLI/MCP upstream error code is `upstream_api_error`; raw bodies are bounded and redacted.

* CLI data output is human-readable by default; pass the global `--json` flag for structured success and runtime-error envelopes
* Library `search_items` / `list_collections` / `library query|collections` return envelopes (`hits`, `has_more`, …), not bare arrays
* `papers search --limit` is **page size** (default 10); use `--per-source` for fan-out
* Search defaults to **compact** hits (no full abstracts); pass `--detail full` / `detail: "full"` for abstracts
* Source wire names: `openalex`, `openreview`, `scholarapi` (old snake_case aliases still deserialize)
* Unbounded search (`limit=0` meaning “all”) removed — page size defaults to 10 (max 50)

### Bug Fixes

* **identity:** never substitute a relevance-ranked cached paper for a failed Zotero key; reject conflicting IDs during annotation/deduplication
* **output:** preserve complementary IDs/PDF routes, Zotero DOI/venue/ISBN/creator details, collection versions, and original-query match evidence
* **read:** select before truncating, preserve UTF-8 chunk cursors, bound all canonical structure paths, and report accurate provenance
* **open:** require exact DOI, arXiv, or canonical URL identity before reusing cached content
* **yams:** pass the JSON flag in the supported position, parse current result envelopes, and verify content before storing an index hash
* **paperseed:** serialize concurrent corpus writers, quarantine corrupt databases, reject ambiguous ids, preserve OA licenses, and verify seed-file integrity
* **paperseed:** index real abstracts/arXiv ids, persist fallback rebuilds, and improve Unicode/stemmed matching

### Security

* Bump `quick-xml` to 0.41 (RUSTSEC-2026-0194 / 0195)

## [2.0.0](https://github.com/trvon/paperbridge/compare/paperbridge-v1.0.1...paperbridge-v2.0.0) (2026-09-07)


### ⚠ BREAKING CHANGES

* **agent:** library totals may be null; structure opens return outlines; MCP query_paper wraps selections in {value}. See docs/design/model-output-remediation.md for migration and limitations.
* **cli:** structured commands now print human-readable output by default. Pass --json for machine-readable success and runtime error envelopes. Paperseed corpus export defaults to BibTeX.

### Features

* **access:** add institutional holdings resolution ([23d3346](https://github.com/trvon/paperbridge/commit/23d334647a174df94409041ff82859052349ec30))
* adding auto complete and update Cargo ([c651c7d](https://github.com/trvon/paperbridge/commit/c651c7dcfa942410e2b6f46567d6a938d4a7651e))
* adding doi / crossref validation ([4e96df0](https://github.com/trvon/paperbridge/commit/4e96df02d72eaef64995b122c9cde42332c55c8b))
* adding paper search router that support api for huggingface and SemanticScholar ([3fdcc8b](https://github.com/trvon/paperbridge/commit/3fdcc8b34e7baa00186234d6ac3b2364a0676df2))
* adding skill command for better agent steering. Fixing format issues ([13f8662](https://github.com/trvon/paperbridge/commit/13f8662ecdd2ccf1990334eaa5eef5f2e3bdfea8))
* agent-first search envelopes, open_paper, and compact hits ([065d8dc](https://github.com/trvon/paperbridge/commit/065d8dc14b7112106e0229829136d331b0782a62))
* ci preparation for easy install ([dace435](https://github.com/trvon/paperbridge/commit/dace4355263e9ef8a72545109d11d1d7eeb8398d))
* cleaning cli commands ([f500a56](https://github.com/trvon/paperbridge/commit/f500a565ee91f542a5b881f806d3a03fe10cc9f7))
* **cli:** make JSON output opt-in ([5a10866](https://github.com/trvon/paperbridge/commit/5a10866c7e129a4706c9264e459b33b9516fc3ad))
* clippy fixes ([a3cb12b](https://github.com/trvon/paperbridge/commit/a3cb12b889037018b794020b7413166c7cd35ab3))
* expanding tests and improving ci ([dbc9b4c](https://github.com/trvon/paperbridge/commit/dbc9b4c125b250b9fc5a8380aa432dd9ee162470))
* expanding tests and improving ci ([ed8fa9d](https://github.com/trvon/paperbridge/commit/ed8fa9d6b4b799414204e97350668dcc7384203d))
* expanding to more source types, improving tests and documentation ([b26febd](https://github.com/trvon/paperbridge/commit/b26febd51e3ba60c60674d09b321b15d427dbca5))
* experimental CRUD management ([c774322](https://github.com/trvon/paperbridge/commit/c774322d8f50e9f0f071df229d492debf3b40f41))
* hello world ([335a32f](https://github.com/trvon/paperbridge/commit/335a32f666d4430e6c14772964fb7ced05b88c2f))
* hybrid routing for local and cloud routes ([aec4a69](https://github.com/trvon/paperbridge/commit/aec4a694f9dc577e9051bd54d865f80357112804))
* improve CLI design philosphy and add embedded skill file ([5452006](https://github.com/trvon/paperbridge/commit/5452006e1d86a94a3612430a9b360ebafafc8384))
* improving config to direct users to local usage ([7028691](https://github.com/trvon/paperbridge/commit/702869161b1c64d806bcc9776bb971388a3ed46b))
* npm install path ([bbefcf3](https://github.com/trvon/paperbridge/commit/bbefcf3f9484d5704ad43dd4a557e055b1321c5f))
* paperseed with yams backend. Experimental update ([cdc1d8c](https://github.com/trvon/paperbridge/commit/cdc1d8c23a524d38512f0fe8bfa917e5e731d23e))
* **paperseed:** replace hand-rolled PDF extractor with pdf-extract ([712ebdd](https://github.com/trvon/paperbridge/commit/712ebdd16d2f0c336cc4953a3dd8394e5704cee5))
* PDF text extraction during import + dual-path caching tests ([30ce857](https://github.com/trvon/paperbridge/commit/30ce8570568ff9f9eb200c7fad2f03ecb7df1ee8))
* **read:** bound and harden paper output ([e07ede3](https://github.com/trvon/paperbridge/commit/e07ede333df8be58b4d277e114b4a199d683e226))
* refinements logs in output and scholarapi add ([be3152f](https://github.com/trvon/paperbridge/commit/be3152fce2ba0c296199204b4ba5da153f7b0586))
* release please and docs update ([9b623f2](https://github.com/trvon/paperbridge/commit/9b623f29d9f58474695a25cbf7ee98ee2e60bbfb))
* **research:** integrate verified YAMS paper workflows ([a777587](https://github.com/trvon/paperbridge/commit/a777587e1cbe8cf156ce12b6243d956309f1f291))
* **retrieval:** BM25F cache index + fix build & saturation ([bc8d4b4](https://github.com/trvon/paperbridge/commit/bc8d4b423f9125529c218622daaa427c20088973))
* search pagination + natural-language cache retrieval ([c9e988d](https://github.com/trvon/paperbridge/commit/c9e988da9ab7cdf2f9e8e4add99c8d597af38f9f))
* **search:** normalize DOI/arXiv queries and tighten title ranking ([311f276](https://github.com/trvon/paperbridge/commit/311f276df7b51e67aea0b6cc3360d1f7ab9dc9d9))
* security fixes ([acd78f5](https://github.com/trvon/paperbridge/commit/acd78f5359be316ce35e587eacac6c9755f5f1f1))
* structured paper return for llm search, documentation cleanups and expanding configuration ([50a3fcf](https://github.com/trvon/paperbridge/commit/50a3fcf7ed3647af142dc2ff120072f62814495e))
* **update:** add paperbridge update and passive release nag ([8cd40f1](https://github.com/trvon/paperbridge/commit/8cd40f1972942f59082badcee01343a59c03dee7))


### Bug Fixes

* **agent:** harden model output contracts ([#35](https://github.com/trvon/paperbridge/issues/35)) ([93a165b](https://github.com/trvon/paperbridge/commit/93a165bd0021b3f5497dcd68de77de699b6f5d88))
* audit round 2 — ranker + yams hardening ([e565352](https://github.com/trvon/paperbridge/commit/e56535206adc77797b18ad6332096c9423842dad))
* cached paper structure returns metadata even without fulltext ([1999b47](https://github.com/trvon/paperbridge/commit/1999b47796a35f6b7b5eecda61bfacc55469974c))
* cargo fmt formatting ([f876fc1](https://github.com/trvon/paperbridge/commit/f876fc1f5d3c74b74a73609e992fd53b10f18bd7))
* **cargo:** cargo update ([8fe8c7b](https://github.com/trvon/paperbridge/commit/8fe8c7bb5e8c50e87f1f3e7c6873bda392ac53b4))
* ci fix for unix installs ([0e2ee79](https://github.com/trvon/paperbridge/commit/0e2ee79d06a96f6b49a2daada32e9e51d7e60e05))
* **dev:** support local opengrep CLI ([565a1ea](https://github.com/trvon/paperbridge/commit/565a1ea1e5175af799a53a3ef7e75c9e9e8d1eb5))
* fixing api to improve reliability of queries ([1b42038](https://github.com/trvon/paperbridge/commit/1b42038236b80d1175dc5b6973dcecc0ff0c29d2))
* fixing mcp search ([79a11f7](https://github.com/trvon/paperbridge/commit/79a11f741e8d943557a47ffc9eaab49e7a6aa8d0))
* harden agent search and open workflows ([fbca02f](https://github.com/trvon/paperbridge/commit/fbca02f563b432ec96297fa549d703c99eaaa47e))
* local improvements and checkpoint ([9d30f72](https://github.com/trvon/paperbridge/commit/9d30f7240894ba97553792aa68c7c708cc59f0ec))
* **open:** require exact cache identity matches ([9299497](https://github.com/trvon/paperbridge/commit/9299497546ff6969e09a08ae01a26548c6f63563))
* **papers:** advertise all 11 sources and harden OpenReview client ([d1ffa0d](https://github.com/trvon/paperbridge/commit/d1ffa0d6039a54fd083c8e2c5bbd7d80f2b33bff))
* **paperseed:** harden local corpus ([5825371](https://github.com/trvon/paperbridge/commit/5825371d713c9be9550903b3b9da17b4e951ced8))
* query norm, docker arg gate, abstract dedup ([4616ffa](https://github.com/trvon/paperbridge/commit/4616ffa6a4e03f91796ec56ae990e85fb182f8e8))
* **query_paper:** rename abstract_note JSON key to abstract ([b43aa50](https://github.com/trvon/paperbridge/commit/b43aa50cbb213224911b2023f6a77a1323cbdb34))
* **search:** gate cache results and update PDF deps ([5122c60](https://github.com/trvon/paperbridge/commit/5122c6007290ad21aa37568342993045085039aa))
* **search:** resolve identifiers and rank query coverage ([eabda62](https://github.com/trvon/paperbridge/commit/eabda626c7ed35ff8eedf3a7fd1ee43f2d0dfd74))
* security fixes ([e0d3869](https://github.com/trvon/paperbridge/commit/e0d38695f2f64dfa762fc3dbfcb1a06e695bdad3))

## [1.0.1](https://github.com/trvon/paperbridge/compare/v1.0.0...v1.0.1) (2026-07-19)


### Bug Fixes

* **paperseed:** harden local corpus ([5825371](https://github.com/trvon/paperbridge/commit/5825371d713c9be9550903b3b9da17b4e951ced8))

## [1.0.0](https://github.com/trvon/paperbridge/compare/v0.11.1...v1.0.0) (2026-07-12)


### ⚠ BREAKING CHANGES

* **cli:** structured commands now print human-readable output by default. Pass --json for machine-readable success and runtime error envelopes. Paperseed corpus export defaults to BibTeX.

### Features

* agent-first search envelopes, open_paper, and compact hits ([065d8dc](https://github.com/trvon/paperbridge/commit/065d8dc14b7112106e0229829136d331b0782a62))
* **cli:** make JSON output opt-in ([5a10866](https://github.com/trvon/paperbridge/commit/5a10866c7e129a4706c9264e459b33b9516fc3ad))
* **research:** integrate verified YAMS paper workflows ([a777587](https://github.com/trvon/paperbridge/commit/a777587e1cbe8cf156ce12b6243d956309f1f291))


### Bug Fixes

* harden agent search and open workflows ([fbca02f](https://github.com/trvon/paperbridge/commit/fbca02f563b432ec96297fa549d703c99eaaa47e))
* **open:** require exact cache identity matches ([9299497](https://github.com/trvon/paperbridge/commit/9299497546ff6969e09a08ae01a26548c6f63563))
* **search:** resolve identifiers and rank query coverage ([eabda62](https://github.com/trvon/paperbridge/commit/eabda626c7ed35ff8eedf3a7fd1ee43f2d0dfd74))

## [0.11.1](https://github.com/trvon/paperbridge/compare/v0.11.0...v0.11.1) (2026-07-02)


### Bug Fixes

* **search:** gate cache results and update PDF deps ([5122c60](https://github.com/trvon/paperbridge/commit/5122c6007290ad21aa37568342993045085039aa))

## [0.11.0](https://github.com/trvon/paperbridge/compare/v0.10.0...v0.11.0) (2026-06-20)


### Features

* **search:** normalize DOI/arXiv queries and tighten title ranking ([311f276](https://github.com/trvon/paperbridge/commit/311f276df7b51e67aea0b6cc3360d1f7ab9dc9d9))


### Bug Fixes

* audit round 2 — ranker + yams hardening ([e565352](https://github.com/trvon/paperbridge/commit/e56535206adc77797b18ad6332096c9423842dad))
* query norm, docker arg gate, abstract dedup ([4616ffa](https://github.com/trvon/paperbridge/commit/4616ffa6a4e03f91796ec56ae990e85fb182f8e8))

## [0.10.0](https://github.com/trvon/paperbridge/compare/v0.9.0...v0.10.0) (2026-05-26)


### Features

* expanding tests and improving ci ([dbc9b4c](https://github.com/trvon/paperbridge/commit/dbc9b4c125b250b9fc5a8380aa432dd9ee162470))
* expanding tests and improving ci ([ed8fa9d](https://github.com/trvon/paperbridge/commit/ed8fa9d6b4b799414204e97350668dcc7384203d))

## [0.9.0](https://github.com/trvon/paperbridge/compare/v0.8.1...v0.9.0) (2026-05-14)


### Features

* **paperseed:** replace hand-rolled PDF extractor with pdf-extract ([712ebdd](https://github.com/trvon/paperbridge/commit/712ebdd16d2f0c336cc4953a3dd8394e5704cee5))

## [0.8.1](https://github.com/trvon/paperbridge/compare/v0.8.0...v0.8.1) (2026-05-12)


### Bug Fixes

* **query_paper:** rename abstract_note JSON key to abstract ([b43aa50](https://github.com/trvon/paperbridge/commit/b43aa50cbb213224911b2023f6a77a1323cbdb34))

## [0.8.0](https://github.com/trvon/paperbridge/compare/v0.7.0...v0.8.0) (2026-05-09)


### Features

* **retrieval:** BM25F cache index + fix build & saturation ([bc8d4b4](https://github.com/trvon/paperbridge/commit/bc8d4b423f9125529c218622daaa427c20088973))

## [0.7.0](https://github.com/trvon/paperbridge/compare/v0.6.0...v0.7.0) (2026-05-08)


### Features

* paperseed with yams backend. Experimental update ([cdc1d8c](https://github.com/trvon/paperbridge/commit/cdc1d8c23a524d38512f0fe8bfa917e5e731d23e))
* PDF text extraction during import + dual-path caching tests ([30ce857](https://github.com/trvon/paperbridge/commit/30ce8570568ff9f9eb200c7fad2f03ecb7df1ee8))
* search pagination + natural-language cache retrieval ([c9e988d](https://github.com/trvon/paperbridge/commit/c9e988da9ab7cdf2f9e8e4add99c8d597af38f9f))


### Bug Fixes

* cached paper structure returns metadata even without fulltext ([1999b47](https://github.com/trvon/paperbridge/commit/1999b47796a35f6b7b5eecda61bfacc55469974c))

## [0.6.0](https://github.com/trvon/paperbridge/compare/v0.5.0...v0.6.0) (2026-04-28)


### Features

* cleaning cli commands ([f500a56](https://github.com/trvon/paperbridge/commit/f500a565ee91f542a5b881f806d3a03fe10cc9f7))
* refinements logs in output and scholarapi add ([be3152f](https://github.com/trvon/paperbridge/commit/be3152fce2ba0c296199204b4ba5da153f7b0586))


### Bug Fixes

* fixing mcp search ([79a11f7](https://github.com/trvon/paperbridge/commit/79a11f741e8d943557a47ffc9eaab49e7a6aa8d0))

## [0.5.0](https://github.com/trvon/paperbridge/compare/v0.4.0...v0.5.0) (2026-04-25)


### Features

* **update:** add paperbridge update and passive release nag ([8cd40f1](https://github.com/trvon/paperbridge/commit/8cd40f1972942f59082badcee01343a59c03dee7))


### Bug Fixes

* **cargo:** cargo update ([8fe8c7b](https://github.com/trvon/paperbridge/commit/8fe8c7bb5e8c50e87f1f3e7c6873bda392ac53b4))
* **papers:** advertise all 11 sources and harden OpenReview client ([d1ffa0d](https://github.com/trvon/paperbridge/commit/d1ffa0d6039a54fd083c8e2c5bbd7d80f2b33bff))

## [0.4.0](https://github.com/trvon/paperbridge/compare/v0.3.0...v0.4.0) (2026-04-16)


### Features

* adding skill command for better agent steering. Fixing format issues ([13f8662](https://github.com/trvon/paperbridge/commit/13f8662ecdd2ccf1990334eaa5eef5f2e3bdfea8))
* clippy fixes ([a3cb12b](https://github.com/trvon/paperbridge/commit/a3cb12b889037018b794020b7413166c7cd35ab3))
* structured paper return for llm search, documentation cleanups and expanding configuration ([50a3fcf](https://github.com/trvon/paperbridge/commit/50a3fcf7ed3647af142dc2ff120072f62814495e))


### Bug Fixes

* security fixes ([e0d3869](https://github.com/trvon/paperbridge/commit/e0d38695f2f64dfa762fc3dbfcb1a06e695bdad3))

## [0.3.0](https://github.com/trvon/paperbridge/compare/v0.2.0...v0.3.0) (2026-04-15)


### Features

* adding auto complete and update Cargo ([c651c7d](https://github.com/trvon/paperbridge/commit/c651c7dcfa942410e2b6f46567d6a938d4a7651e))
* expanding to more source types, improving tests and documentation ([b26febd](https://github.com/trvon/paperbridge/commit/b26febd51e3ba60c60674d09b321b15d427dbca5))
* improve CLI design philosphy and add embedded skill file ([5452006](https://github.com/trvon/paperbridge/commit/5452006e1d86a94a3612430a9b360ebafafc8384))
* security fixes ([acd78f5](https://github.com/trvon/paperbridge/commit/acd78f5359be316ce35e587eacac6c9755f5f1f1))


### Bug Fixes

* ci fix for unix installs ([0e2ee79](https://github.com/trvon/paperbridge/commit/0e2ee79d06a96f6b49a2daada32e9e51d7e60e05))

## [0.2.0](https://github.com/trvon/paperbridge/compare/v0.1.0...v0.2.0) (2026-04-14)


### Features

* adding paper search router that support api for huggingface and SemanticScholar ([3fdcc8b](https://github.com/trvon/paperbridge/commit/3fdcc8b34e7baa00186234d6ac3b2364a0676df2))
* npm install path ([bbefcf3](https://github.com/trvon/paperbridge/commit/bbefcf3f9484d5704ad43dd4a557e055b1321c5f))

## 0.1.0 (2026-04-10)


### Features

* adding doi / crossref validation ([4e96df0](https://github.com/trvon/paperbridge/commit/4e96df02d72eaef64995b122c9cde42332c55c8b))
* ci preparation for easy install ([dace435](https://github.com/trvon/paperbridge/commit/dace4355263e9ef8a72545109d11d1d7eeb8398d))
* experimental CRUD management ([c774322](https://github.com/trvon/paperbridge/commit/c774322d8f50e9f0f071df229d492debf3b40f41))
* hello world ([335a32f](https://github.com/trvon/paperbridge/commit/335a32f666d4430e6c14772964fb7ced05b88c2f))
* hybrid routing for local and cloud routes ([aec4a69](https://github.com/trvon/paperbridge/commit/aec4a694f9dc577e9051bd54d865f80357112804))
* improving config to direct users to local usage ([7028691](https://github.com/trvon/paperbridge/commit/702869161b1c64d806bcc9776bb971388a3ed46b))
* release please and docs update ([9b623f2](https://github.com/trvon/paperbridge/commit/9b623f29d9f58474695a25cbf7ee98ee2e60bbfb))


### Bug Fixes

* cargo fmt formatting ([f876fc1](https://github.com/trvon/paperbridge/commit/f876fc1f5d3c74b74a73609e992fd53b10f18bd7))
* fixing api to improve reliability of queries ([1b42038](https://github.com/trvon/paperbridge/commit/1b42038236b80d1175dc5b6973dcecc0ff0c29d2))
* local improvements and checkpoint ([9d30f72](https://github.com/trvon/paperbridge/commit/9d30f7240894ba97553792aa68c7c708cc59f0ec))
