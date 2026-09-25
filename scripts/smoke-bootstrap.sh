#!/usr/bin/env bash
# Smoke tests for pkgq bootstrap validation (guide scenarios 1, 2 and 6).
#
# Covers:
#   Stage 1:  llama-server path with GGUFs present -> validation messages on
#             stderr with PKGQ_FORCE_BOOTSTRAP=1, no downloads attempted;
#             plain run afterwards stays silent (warm marker).
#   Stage 2:  a listener on port 43210 is reported active (avoids spawning
#             duplicate servers); no downloads triggered either.
#   Stage 3:  PKGQ_NO_BOOTSTRAP=1 disables bootstrap output entirely, even
#             combined with PKGQ_FORCE_BOOTSTRAP=1.
#   Stage 4:  optional (SMOKE_FULL=1): with a fake server on 43210, `pkgq
#             index` still exits 0 (embeddings degrade to candle instead of
#             crashing). May download candle weights (~2.3 GB) on first run.
#
# Requirements: llama-server available (PATH or ~/.local/bin) and the bge-m3
# GGUF already in ~/models, matching the reference machine layout.
#
# Usage: scripts/smoke-bootstrap.sh [path-to-pkgq-binary]
set -u -o pipefail

BIN="${1:-${PKGQ_BIN:-target/release/pkgq}}"
PASS=0
FAIL=0
SRV=""

note() { printf '%s\n' "$*"; }
ok() { printf 'PASS: %s\n' "$*"; PASS=$((PASS + 1)); }
bad() { printf 'FAIL: %s\n' "$*"; FAIL=$((FAIL + 1)); }
say() { printf '\n== %s ==\n' "$*"; }

port_active() { (echo >"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

# Run a command and print only its stderr on stdout.
run_stderr() { "$@" 2>&1 >/dev/null; }

trap '[[ -n "$SRV" ]] && kill "$SRV" 2>/dev/null' EXIT

if [[ ! -x "$BIN" ]]; then
  note "binary '$BIN' not found; building (cargo build --release)..."
  cargo build --release || { note "build failed"; exit 1; }
fi

if ! command -v llama-server >/dev/null 2>&1 && [[ ! -x "$HOME/.local/bin/llama-server" ]]; then
  note "llama-server not found; these smoke stages expect the llama-server layout"
  exit 1
fi

wait_port() { # wait_port <port>
  local _i
  for _i in $(seq 1 25); do
    port_active "$1" && return 0
    sleep 0.2
  done
  return 1
}

# ---------------------------------------------------------------- Stage 1
say "Stage 1: llama-server path, GGUFs present (PKGQ_FORCE_BOOTSTRAP=1)"
OUT=$(run_stderr env PKGQ_FORCE_BOOTSTRAP=1 "$BIN" list --compact)

if grep -q "validando entorno de inferencia" <<<"$OUT"; then
  ok "validation banner shown"
else
  bad "validation banner missing"
fi

if port_active 43210; then
  if grep -q "puerto 43210 para embeddings activo" <<<"$OUT"; then
    ok "pre-existing port 43210 reported active"
  else
    bad "port 43210 already active but not reported as such"
  fi
else
  if grep -q "puerto 43210 inactivo; modelo GGUF listo" <<<"$OUT"; then
    ok "port 43210 inactive, GGUF ready, no server spawned"
  else
    bad "unexpected port 43210 message"
  fi
fi

if grep -q "puerto 43211 (LFM2.5) inactivo; GGUF LFM2.5-230M-F16.gguf (presente: true)" <<<"$OUT"; then
  ok "LFM2.5 GGUF reported present"
else
  bad "LFM2.5 message wrong"
fi

if grep -q "puerto 43212 (K2) inactivo; GGUF K2-Horizon-1B-BF16.gguf (presente: true)" <<<"$OUT"; then
  ok "K2 GGUF reported present"
else
  bad "K2 message wrong"
fi

if grep -q "descargando" <<<"$OUT"; then
  bad "unexpected download attempted"
else
  ok "no downloads attempted"
fi

say "Stage 1b: plain run stays silent (warm marker)"
OUT2=$(run_stderr "$BIN" list --compact)
if grep -Eq "bootstrap:|llama-server:|Candle" <<<"$OUT2"; then
  bad "bootstrap noise without PKGQ_FORCE_BOOTSTRAP"
else
  ok "no bootstrap output on warm run"
fi

# ---------------------------------------------------------------- Stage 2
say "Stage 2: listener on 43210 reported active (fake server)"
python3 -m http.server 43210 --bind 127.0.0.1 >/dev/null 2>&1 &
SRV=$!
if wait_port 43210; then
  OUT=$(run_stderr env PKGQ_FORCE_BOOTSTRAP=1 "$BIN" list --compact)
  if grep -q "puerto 43210 para embeddings activo (evitando doble llamada)" <<<"$OUT"; then
    ok "active port avoids duplicate server setup"
  else
    bad "active-port message missing"
  fi
  if grep -q "descargando" <<<"$OUT"; then
    bad "download attempted despite active port"
  else
    ok "no downloads with active port"
  fi
else
  bad "test listener did not come up on 43210"
fi
kill "$SRV" 2>/dev/null
wait "$SRV" 2>/dev/null
SRV=""

# ---------------------------------------------------------------- Stage 3
say "Stage 3: PKGQ_NO_BOOTSTRAP=1 disables bootstrap"
OUT=$(run_stderr env PKGQ_NO_BOOTSTRAP=1 PKGQ_FORCE_BOOTSTRAP=1 "$BIN" list --compact)
if grep -Eq "bootstrap:|llama-server:|Candle" <<<"$OUT"; then
  bad "bootstrap ran despite PKGQ_NO_BOOTSTRAP=1"
else
  ok "bootstrap fully skipped"
fi

# ---------------------------------------------------------------- Stage 4
say "Stage 4 (SMOKE_FULL): index degrades on fake embeddings server"
if [[ "${SMOKE_FULL:-0}" == "1" ]]; then
  python3 -m http.server 43210 --bind 127.0.0.1 >/dev/null 2>&1 &
  SRV=$!
  if wait_port 43210; then
    if "$BIN" index --manager dpkg --compact >/dev/null 2>&1; then
      ok "pkgq index exits 0 with a fake embeddings server"
    else
      bad "pkgq index failed with a fake embeddings server"
    fi
  else
    bad "test listener did not come up on 43210"
  fi
  kill "$SRV" 2>/dev/null
  wait "$SRV" 2>/dev/null
  SRV=""
else
  note "SKIP: set SMOKE_FULL=1 to run stage 4 (may download candle weights ~2.3 GB on first run)"
fi

# ---------------------------------------------------------------- Summary
say "Summary"
note "passed: $PASS, failed: $FAIL"
[[ "$FAIL" -eq 0 ]] || exit 1
