# Contributing to paperbridge

Thanks for contributing.

## Prerequisites

- Rust stable toolchain
- `cargo-deny` (`cargo install cargo-deny`)

## Local setup

```bash
./setup.sh
paperbridge config init --interactive
paperbridge config validate
```

## Git hooks

The repo ships pre-commit hooks in `.githooks/`. Activate them with:

```bash
git config core.hooksPath .githooks
```

This runs automatically during `./setup.sh`. The pre-commit hook checks formatting, clippy, cargo-deny, and tests before each commit.

## Development workflow

1. Create a branch for your change.
2. Implement changes with tests.
3. Run all required checks locally (or rely on the pre-commit hook).
4. Open a pull request with a clear summary and rationale.

## Required checks

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy -- -D warnings
cargo check --tests
cargo deny check advisories bans sources
```

## Code guidelines

- Keep modules focused and testable.
- Prefer explicit types and clear error messages.
- Avoid broad refactors unrelated to the current change.
- Do not add deep coupling between Vox and paperbridge.

## Test guidance

- Add unit tests for parsing and pure logic.
- Use mocked HTTP integration tests for Zotero API behavior.
- If output contracts change, update golden fixtures in `tests/golden/`.

## Security and dependencies

- Use `cargo deny` to catch advisories and source policy issues.
- Keep dependencies minimal and prefer actively maintained crates.
