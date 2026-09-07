# Model-output audit remediation

This PR addresses the model-facing audit through regression tests, not by adding
more overlapping retrieval tools. The authoritative contract remains
[llm-interface.md](llm-interface.md).

## Finding → change

| Finding | Remediation / coverage |
|---|---|
| Wrong cached document substituted for a failed Zotero key | Exact-identity reads only; regression with an unrelated paper mentioning the attachment key |
| Cache annotations cross conflicting DOI identities | Shared compatible-identity checks; title fallback requires author corroboration |
| Oversized structure/metadata/error output | Bounded canonical views, explicit omissions, streaming bounded upstream error reads, 64 KiB MCP wire guard |
| Selector applied after truncation | Select on complete structure, then paginate selected strings or bound subtrees |
| Chunks lose continuation | Independent `chunks_page` with UTF-8 byte cursor and completeness |
| Offset expands/reorders candidate pool | Fixed per-source prefix; `count_kind=candidate_window`; unchanged upstream prefixes produce stable pages |
| Fabricated library totals | Sentinel-based `has_more`; nullable unknown totals instead of invented cardinality |
| PMID/missing cache placeholders | Prefer supported URL identity for PMID records; explicit unsupported/missing-ID errors; metadata retrieval status |
| Dedupe loses DOI/arXiv/PDF metadata | Merge complementary metadata; preserve externally usable PDF routes alongside cache IDs |
| False provenance and weak exact-title explanations | Origin/retrieval/parser separated; classify against original query, not acronym expansion |
| Zotero citation and collection version loss | Preserve DOI, venue, ISBN, creator details, and collection versions |
| Untyped MCP transport and weak schemas | Output schemas, structured content, object-root selection envelope, required-target/enum/range input constraints |
| Flat tool/context load | Opt-in six-tool `core` profile with absent routes uncallable; `full` remains default |
| Wrong server name/version | Explicit Paperbridge package identity, verified over stdio |
| CLI/MCP error drift | Shared bounded/redacted error envelope with safe recovery suggestions |
| Projection damages persisted citations | Mirror original metadata before applying compact title/author/abstract caps |
| Combined views discard Zotero structure metadata | Preserve item-backed parser/metadata path when fulltext and structure are requested together |

## Client migration

- Library `total_count` can be null. Inspect `count_kind` and page via `has_more` /
  `next_offset`, not arithmetic based on a guessed total.
- Paper search never expands the candidate prefix while paging. To search more
  deeply, increase `limit_per_source` / `--per-source` (up to 200) and restart at
  offset 0. This is a bounded stateless window, **not** an upstream snapshot;
  changes at providers can still reorder results between calls.
- `open_paper want=["structure"]` returns an outline. Use its available selectors
  to fetch section text. Follow `structure_page.next_offset` for string
  selections, `fulltext.next_offset` for fulltext, and `chunks_page.next_offset`
  for chunks. Offset units are UTF-8 bytes, while budgets/counts are characters.
- Inspect `metadata_status` and `metadata_page`; `identifier_only` means a
  descriptor, not retrieved bibliographic evidence. Each requested view has its
  own character budget; identifiers are not silently truncated.
- MCP `query_paper` now returns `{ "value": ... }`. CLI selected JSON is unchanged.
- Prefer `structuredContent`. Compact text remains for old clients; the 64 KiB
  MCP result cap counts **both**. Overflow returns no partial success: reduce the
  requested view using the returned recovery action. Canonical view bounds also
  apply to CLI opens; low-level CLI exports remain explicit full-content paths.
- Runtime execution errors use `isError:true`; invalid inputs use protocol
  invalid-params with structured data. Shared API error code is
  `upstream_api_error`. `recovery` entries are suggestions, never authorization.
- Use `paperbridge serve --profile core` for discovery/read only. The default
  `full` profile preserves all existing tool names. Prompts are optional; do not
  load the full guide unless needed.

## Verification

`tests/mcp_contract.rs` drives a real stdio server against localhost fixtures:
manifest/profile, server identity, schema-valid metadata, bounded/redacted errors,
chunk cursors, structure budgets, and invalid parameters. Unit regressions cover
identity conflicts, merged identifiers, fixed pagination, sentinel totals,
original-query match explanations, late selectors, Unicode, provenance, provider
error conversion, and preservation of full mirrored citations. Stdio coverage also
checks that adding fulltext does not change the selected Zotero structure metadata.
`tests/output_efficiency.rs` keeps the default compact search size guardrail.

Required gates: `cargo test`, `cargo check`, `cargo clippy -- -D warnings`,
`cargo fmt --check`, `cargo check --tests`. No live paper API calls in tests.

Deferred by design: a native PMID resolver and persistent cross-call upstream
snapshots. Their absence is explicit rather than misrepresented as successful
retrieval or globally complete search results.
