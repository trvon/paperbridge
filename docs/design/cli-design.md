# paperbridge — CLI Design Methodology

## Purpose

This document defines the required design methodology for the `paperbridge`
CLI. It exists to prevent command sprawl, flattening of unrelated concerns,
weak help output, and error messages that leave users or agents at dead
ends.

This is not a wishlist. Any change to CLI commands, help text, aliases,
examples, error copy, or user flow must be reviewed against this document.

## Goals

- Keep the CLI learnable without memorizing examples.
- Make the command graph reflect the real product mental model (Zotero
  library ↔ external paper indexes ↔ MCP surface).
- Prefer layered workflows over oversized command groups.
- Make non-interactive and agent use first-class.
- Ensure every failure teaches the next valid action.
- Keep one canonical path per user intent.

## Design Principles

### 1. One canonical path per intent

- Each user intent has exactly one preferred command path.
- Aliases may exist for migration, but they must not compete with the
  canonical command in help, docs, examples, or the SKILL.
- Hidden aliases (`#[command(hide = true)]`) are acceptable for backwards
  compatibility.
- New features must not be introduced behind aliases first.

Required review question: if a user asks "what command do I run for this?",
is there exactly one preferred answer?

### 2. Layer commands by workflow stage

Top-level commands represent stable domains, not implementation detail.
Within a domain, subcommands follow an explicit user flow:

1. Discover what exists (`library query`, `library collections`)
2. Inspect details (`library read`)
3. Mutate (`item create|update|delete`, `collection create|update|delete`)
4. Administer (`config`, `status`)

Do not mix unrelated lifecycle stages at a flat level when a subtree would
reduce guesswork. If a command family needs many examples to explain
itself, the command graph is likely carrying too much at one level. Prefer
narrow subtrees over adding more flags to a crowded root command.

### 3. Separate retrieval from decisioning

- Raw lookup and ranked/contextual recommendation are different user
  intents.
- Retrieval answers "what exists?" (`library query`, `papers search`).
- Disambiguate by noun when the verb is the same: `library query` returns
  items already in the Zotero library; `papers search` hits external
  indexes (arXiv, HuggingFace Papers, Semantic Scholar, Crossref).
- Help text must explain the difference directly and briefly.

### 4. Help teaches the graph, not compensates for it

- `--help` at every level is enough to discover the next layer.
- Examples reinforce the command structure; they are not the only way to
  understand it.
- Prefer short descriptions that distinguish neighboring commands.
- Use `after_help` sparingly and only for essential examples.
- If the help block is doing the work that a missing subtree should do,
  fix the command structure instead of adding more prose.

Every command group must make its immediate next options obvious, and help
for sibling commands must explain why each exists.

### 5. Errors are guiding, actionable, deterministic

Every user-facing runtime error does three things:

1. State what failed.
2. State why, if known.
3. Give 1–2 exact next commands to recover.

Good error shape:

```text
Cloud backend requires an api_key, but none is configured.
Try:
  paperbridge config init --interactive
  paperbridge config set api_key <your-key>
```

Rules:

- Do not emit vague failures like "not found" or "invalid input" without
  context.
- Do not rely on users to infer whether they should run `config`, `status`,
  `config init`, or `--help`.
- If valid candidates are known (collection keys, item keys, backend
  modes), print them.

### 6. Validate at parse time when possible

- Prefer typed enums and clap validation over free-form strings.
- Invalid sources, shells, backend modes, snippet targets must fail during
  argument parsing.
- Invalid invocations return non-zero exit status. Do not accept invalid
  input and exit 0 with a warning.

Current examples: `--shell`, `--target`, `--sources` all use
`clap::ValueEnum`.

### 7. Non-interactive behavior is first-class

- Any workflow that may be used by scripts, agents, or MCP hooks has a
  deterministic non-interactive path.
- If multiple choices are possible, the CLI provides a flag to select the
  target explicitly.
- If selection cannot be inferred safely, fail with an actionable message
  and show the valid selectors.

Interactive prompts (`config init --interactive`) are allowed only as a
convenience layer over an explicit non-interactive contract
(`config init` with compiled defaults + `config set`).

### 8. Machine-readable output is stable and complete

- JSON output (the default for data-returning commands like `library
  query`, `library read`, `papers search`, `item create`) exposes
  identifiers and state needed for follow-up automation.
- Canonical IDs (Zotero item keys, collection keys, DOIs) are always
  present; display names alone are not enough.
- If human output distinguishes local vs. remote, active vs. inactive, or
  cloud vs. local backend, JSON exposes that state explicitly.

### 9. Derive metadata from the real command graph

- Do not maintain separate, stale command taxonomies by hand.
- Shell completions are generated from `clap::CommandFactory`
  (`paperbridge completions <shell>`).
- Future doc generation (man pages, markdown reference) must be driven by
  the same canonical CLI structure.

### 10. Verify the built CLI surface

- Do not update help text, docs, migration notes, or error suggestions for
  a command path until that path is confirmed in the built CLI.
- Source-level clap changes are not sufficient evidence on their own;
  verify the actual compiled command surface.
- Preferred checks:
  - `cargo run -- <group> --help` while a change is in flight.
  - `tests/cli_surface.rs` — snapshot assertions for the top-level
    `--help` and the visible-vs-hidden invariant.
- If a new command path fails to compile or is absent from the built CLI,
  roll back any help, SKILL, or recovery text that teaches it until the
  path is real.

### 11. Preserve backwards compatibility deliberately

- Compatibility aliases and deprecated paths are intentional, documented,
  and time-bounded.
- When keeping a legacy path, mark the canonical replacement in the
  deprecation warning emitted at runtime and in the `after_help` of the
  hidden variant.
- Do not add new duplicate entrypoints unless there is a migration plan
  with a removal version.

Current legacy aliases (emit `warn!` on invocation, removal targeted for
0.4.0): `query`, `collections`, `read`, `read-search`, `create-item`,
`update-item`, `delete-item`, `validate-item`, `create-collection`,
`update-collection`, `delete-collection`, `backend-info`, `search-papers`,
`resolve-doi`.

## Required Review Checklist

Use this checklist for every CLI surface change.

- [ ] Is there one canonical command path for the user intent?
- [ ] Does the command sit at the correct layer of the workflow?
- [ ] Does `--help` make the next step obvious without reading source or
      docs?
- [ ] Are structured inputs typed and parser-validated?
- [ ] Does the failure path print exact next commands?
- [ ] Is there a deterministic non-interactive path?
- [ ] Was the actual built CLI surface verified (`cargo run -- <path>
      --help`) for any new or renamed command?
- [ ] Does JSON output expose canonical identifiers and state required
      for automation?
- [ ] Are aliases or deprecated paths minimized and clearly treated as
      legacy (hidden + deprecation warning)?
- [ ] Did this change reduce, preserve, or worsen top-level command
      sprawl?

If any answer is "worsen" or "no", redesign before merging unless there is
an explicitly documented exception in the PR description.

## Preferred CLI Flow Patterns

### Discovery-first flow

Use when a resource may or may not exist locally.

```text
library query → library read
```

### Mutation flow

Use when changing remote (Zotero) state.

```text
item validate → item create | item update → (server returns key + version)
```

Validation must run before `item create`/`item update` for any structured
input. Zotero uses optimistic concurrency — `update` and `delete` require
the `version` from the last fetch; a `412` response must surface with a
hint to re-fetch.

### External-to-local flow

Use when promoting an external paper into the Zotero library.

```text
papers search → papers resolve-doi → item validate → item create
```

## Canonical Command Map

| Domain | Canonical path | Legacy alias (hidden) |
|---|---|---|
| MCP server | `paperbridge serve` | — |
| Shell completions | `paperbridge completions <shell>` | — |
| Config | `paperbridge config {init,path,validate,get,set,resolve-user-id,snippet}` | — |
| Status / diagnostics | `paperbridge status` | `backend-info` |
| Local Zotero reads | `paperbridge library {query,collections,read,read-search}` | `query`, `collections`, `read`, `read-search` |
| Item writes | `paperbridge item {create,update,delete,validate}` | `create-item`, `update-item`, `delete-item`, `validate-item` |
| Collection writes | `paperbridge collection {create,update,delete}` | `create-collection`, `update-collection`, `delete-collection` |
| External paper search | `paperbridge papers {search,resolve-doi}` | `search-papers`, `resolve-doi` |

## Current-state Audit

Snapshot taken while landing the methodology. Gaps tracked as follow-ups;
not blockers for this refactor.

| Principle | Status | Notes |
|---|---|---|
| 1. Canonical path per intent | ✅ Enforced by this refactor | Legacy aliases hidden + warn on use. |
| 2. Layer by workflow stage | ✅ Enforced | Domain subtrees replace flat list. |
| 3. Retrieval vs. decisioning | ✅ Enforced | `library query` vs. `papers search`. |
| 4. Help teaches the graph | ✅ Each group has short sibling descriptions. | |
| 5. Guiding errors | 🟡 Partial | `try: ...` hints added in this refactor; further audit due in 0.3.x. |
| 6. Parse-time validation | ✅ `--sources` promoted to `Vec<PaperSource>`. | |
| 7. Non-interactive first-class | ✅ | All `config init` actions have non-interactive defaults. |
| 8. JSON output complete | 🟡 Present for data commands; no explicit `--json` flag on diagnostic commands (`status`, `config validate`). | Follow-up: add `--json` to diagnostics. |
| 9. Derive metadata from clap | ✅ | Completions via `CommandFactory`; no hand-maintained taxonomy. |
| 10. Verify built surface | ✅ | `tests/cli_surface.rs` added. |
| 11. Deliberate back-compat | ✅ | Legacy aliases time-bounded to 0.4.0. |

## Follow-ups (not in this refactor)

- Add `--json` to `status` and `config validate` for machine consumers.
- Build `paperbridge internal dump-graph` that emits a JSON description of
  the clap command tree for external doc generators.
- Remove hidden legacy aliases in 0.4.0 (CHANGELOG entry required).

## Anti-Patterns

- Multiple top-level nouns for the same concept (`query` and `search` at
  the root).
- Large flat command enums that mix discovery, activation, mutation,
  sharing, and maintenance.
- Free-form `String` flags for structured modes (fixed in this refactor).
- Error messages that require the user to guess the next command.
- Help text that depends on long example dumps to explain basic
  navigation.
- Interactive-only selection for workflows that agents or scripts need.
- Separate manual command category lists that can drift from the real
  parser.
