#!/usr/bin/env bash
# Model-check MergeFetchDecide.tla with TLC.
#
# The main config must pass. Each seeded-bug and witness config must fail
# on its expected invariant; any other outcome (pass, parse error, wrong
# invariant, deadlock, Java error) fails this script.
#
# Env:
#   TLA2TOOLS_JAR   use this jar instead of downloading (hash still checked)
#   TLA_CACHE_DIR   download cache (default: $XDG_CACHE_HOME/paperbridge/tla)
#   TLC_WORKERS     TLC -workers for the main config (default: 1); bug and
#                   witness configs always use 1 so the first violation
#                   reported is deterministic
set -euo pipefail

TLA_VERSION="v1.7.4"
TLA_SHA256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
TLA_URL="https://github.com/tlaplus/tlaplus/releases/download/${TLA_VERSION}/tla2tools.jar"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
spec="MergeFetchDecide.tla"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

check_jar() {
  local got
  got="$(sha256 "$1")"
  if [[ "$got" != "$TLA_SHA256" ]]; then
    echo "run.sh: $1: sha256 $got, expected $TLA_SHA256" >&2
    return 1
  fi
}

if [[ -n "${TLA2TOOLS_JAR:-}" ]]; then
  jar="$TLA2TOOLS_JAR"
else
  cache="${TLA_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/paperbridge/tla}"
  jar="$cache/tla2tools-${TLA_VERSION}.jar"
  if [[ ! -f "$jar" ]]; then
    mkdir -p "$cache"
    tmp="$(mktemp "$cache/tla2tools.XXXXXX")"
    echo "run.sh: downloading $TLA_URL"
    curl -fsSL --retry 3 -o "$tmp" "$TLA_URL"
    check_jar "$tmp" || { rm -f "$tmp"; exit 1; }
    mv "$tmp" "$jar"
  fi
fi
check_jar "$jar"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

failures=0

# run_tlc <cfg> <expected invariant or "">
run_tlc() {
  local cfg="$1" expect="$2" log rc=0 start end workers=1
  [[ -z "$expect" ]] && workers="${TLC_WORKERS:-1}"
  log="$workdir/${cfg%.cfg}.log"
  start=$(date +%s)
  (cd "$here" && java -XX:+UseParallelGC -cp "$jar" tlc2.TLC \
      -workers "$workers" -metadir "$workdir/meta-${cfg%.cfg}" \
      -config "$cfg" "$spec") >"$log" 2>&1 || rc=$?
  end=$(date +%s)
  local stats
  stats="$(grep -E 'distinct states found' "$log" | tail -1 || true)"
  local depth
  depth="$(grep -E 'depth of the complete state graph' "$log" | tail -1 || true)"

  if [[ -z "$expect" ]]; then
    if [[ $rc -eq 0 ]] && grep -q 'Model checking completed. No error has been found.' "$log"; then
      echo "PASS  $cfg: no error ($stats; $depth; $((end - start)) s)"
      return
    fi
  else
    if [[ $rc -ne 0 ]] && grep -qE "^Error: Invariant ${expect} is violated\.$" "$log"; then
      echo "PASS  $cfg: rejected on $expect as expected ($stats; $((end - start)) s)"
      return
    fi
  fi
  echo "FAIL  $cfg: exit $rc, expected ${expect:-no error}" >&2
  tail -40 "$log" >&2
  failures=$((failures + 1))
}

run_tlc MergeFetchDecide.cfg ""
# Seeded bugs: each must be rejected.
run_tlc MergeFetchDecide_BugStaleSnapshot.cfg EdgeFaithful
run_tlc MergeFetchDecide_BugMergeOverwrite.cfg NoDecisionLost
run_tlc MergeFetchDecide_BugStoreRoot.cfg ReplayIdempotent
run_tlc MergeFetchDecide_BugStoreRootUndo.cfg EdgeFaithful
run_tlc MergeFetchDecide_BugSplitCommit.cfg CursorImpliesEdges
# Witnesses: the scenarios the invariants guard are reachable.
run_tlc MergeFetchDecide_WitnessNeedsDecision.cfg NeverNeedsDecision
run_tlc MergeFetchDecide_WitnessSelfLoop.cfg NeverResolvedSelfLoop

if [[ $failures -ne 0 ]]; then
  echo "run.sh: $failures check(s) failed" >&2
  exit 1
fi
echo "run.sh: all checks passed"
