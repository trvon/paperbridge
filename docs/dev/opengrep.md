# OpenGrep static-analysis profile

Paperbridge keeps an OpenGrep-compatible local rule pack under `tools/opengrep/rules`.
OpenGrep is a Semgrep fork, so the same YAML rules can be run by OpenGrep or by
Semgrep as a local fallback.

## Install OpenGrep

```bash
curl -fsSL https://raw.githubusercontent.com/opengrep/opengrep/main/install.sh | bash
```

OpenGrep also publishes release binaries on GitHub.

## Run

```bash
scripts/dev/run_opengrep.sh
OPENGREP_PROFILE=audit scripts/dev/run_opengrep.sh src
OPENGREP_PROFILE=public scripts/dev/run_opengrep.sh src crates
OPENGREP_PROFILE=all scripts/dev/run_opengrep.sh src crates tests
```

Outputs:

- `.artifacts/opengrep/opengrep-paperbridge-<profile>.json`
- `.artifacts/opengrep/opengrep-paperbridge-<profile>.sarif`

If `opengrep` is not installed, the wrapper uses `semgrep` when available. Set
`OPENGREP_BIN=/path/to/opengrep` to force a specific binary.

Useful knobs:

```bash
OPENGREP_STRICT=1 scripts/dev/run_opengrep.sh src crates
OPENGREP_TIMEOUT=120 scripts/dev/run_opengrep.sh src crates
OPENGREP_OUT_DIR=.artifacts/opengrep/pr scripts/dev/run_opengrep.sh
OPENGREP_BASELINE_COMMIT=HEAD~1 scripts/dev/run_opengrep.sh
```

## Profiles

- `default`: local paperbridge-specific Rust rules + `p/rust` community pack.
- `audit`: higher-noise audit checks for unsafe, secrets, and security patterns.
- `public`: run curated online registry packs: `p/default`, `p/security-audit`, `p/trailofbits`, `p/rust`.
- `all`: run every local profile.
- Any path or registry entry can be passed as `OPENGREP_PROFILE=/path/to/rules` or `OPENGREP_PROFILE=p/<pack>`.

## Curated local rules

### default profile

- `paperbridge-rust-unwrap-in-production`: flags `unwrap()` outside `#[cfg(test)]` blocks.
- `paperbridge-rust-expect-empty-message`: flags unhelpful `expect("...")` messages.
- `paperbridge-rust-print-in-library`: flags `println!`/`eprintln!` in library code.
- `paperbridge-rust-to-string-lossy`: flags silent Unicode loss via `to_string_lossy()`.
- `paperbridge-rust-file-read-without-context`: flags `unwrap()`/`expect()` on file I/O without context.

### audit profile

- `paperbridge-audit-unsafe-block`: flags `unsafe` blocks for manual review.
- `paperbridge-audit-hardcoded-secret`: flags potential hardcoded tokens/secrets.
- `paperbridge-audit-reqwest-no-timeout`: flags HTTP clients without explicit timeouts.

## Git hook

The tracked pre-commit hook in `.githooks/pre-commit` calls `.githooks/opengrep-pre-commit`.
It is opportunistic: if `opengrep` or local rules are missing, it skips cleanly.
When rules change, it validates the `default` and `audit` profiles. When `.rs` files are staged,
it scans only those staged files with the local `default` profile.

By default findings are advisory so legacy findings do not block commits. To make findings blocking locally:

```bash
PAPERBRIDGE_OPENGREP_STRICT=1 git commit
```

To use the tracked hooks in a clone:

```bash
git config core.hooksPath .githooks
```
