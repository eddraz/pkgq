# pkgq

[![CI](https://github.com/eddraz/pkgq/actions/workflows/ci.yml/badge.svg)](https://github.com/eddraz/pkgq/actions/workflows/ci.yml)

Inventory and search OS applications across package managers, outputting JSON.
Written in Rust; every external command runs exclusively through `bash -c`.

Primary target: Debian Linux. Other managers are supported opportunistically
(CI matrix verifies Arch/pacman, Fedora/dnf and macOS/brew on every push).

## Usage

```bash
pkgq list [--manager m1,m2] [--compact]
pkgq search <query> [--manager m1,m2] [--compact] [--installed-only] [--available-only]
pkgq outdated [--manager m1,m2] [--compact]
pkgq search <query> [--min-confidence 0..1]  # cuts the weak tail
```

- `list` — every application currently installed on the system.
- `search` — the **union** of the installed inventory and each manager's
  catalog results. Whitespace-separated tokens are scored, not hard-filtered:
  every token hit adds relevance (name outweighs description, whole-word
  hits outweigh substrings, a phrase inside the name outranks everything),
  zero-score results are dropped and `search` orders best-match first.
  Installed apps are found even when a manager's remote search does not
  surface them.
- With `--installed-only` the search takes a **fast path**: only the local
  inventories are consulted (no remote catalog queries), so it works offline.
  In that mode installed debs are attributed to `dpkg` (the installed-package
  source of truth), so `--manager apt --installed-only` yields nothing — use
  `--manager dpkg` or omit `--manager`.
- In normal searches an installed deb reported by both `dpkg` and `apt` is
  deduplicated and stays attributed to `dpkg`.
- `outdated` — installed applications with a newer version available.
  `version` keeps the installed one and `available_version` carries the
  candidate. Covers installed applications only (flatpak runtimes and
  extensions are not listed); dpkg is covered through apt.
- `--manager` — restrict to a comma-separated subset of `apt,dpkg,flatpak,snap,brew,pacman,dnf`.
- Output is always JSON: pretty-printed by default, single line with `--compact`.
- Exit code is `0` even when a manager fails; failures are reported in the `errors` array.

## JSON contract (v1)

```json
{
  "command": "search",
  "query": "curl",
  "managers_detected": ["apt", "dpkg", "flatpak", "snap", "brew"],
  "generated_at": "2026-02-14T10:00:00Z",
  "count": 2,
  "results": [
    {
      "name": "curl",
      "manager": "apt",
      "installed": true,
      "version": "8.14.1-2+deb13u5",
      "description": "command line tool for transferring data with URL syntax",
      "usage": "curl",
      "install": "sudo apt install curl",
      "installed_bytes": 530432,
      "download_bytes": null,
      "homepage": "https://curl.se/",
      "license": null,
      "origin": null,
      "arch": "amd64",
      "maintainer": "Debian Curl Maintainers <team+curl@tracker.debian.org>",
      "section": "web",
      "depends": "libcurl4t64 (= 8.21.0-2~bpo13+1), libc6 (>= 2.34), zlib1g (>= 1:1.1.4)",
      "install_date": null,
      "available_version": null
    }
  ],
  "errors": []
}
```

Field notes:

- `manager` — one of `apt`, `dpkg`, `flatpak`, `snap`, `brew`, `pacman`, `dnf`.
- Installed `.deb` packages are attributed to `dpkg` (the database apt uses);
  catalog-only search results are attributed to `apt`.
- `usage` — the primary executable the package ships (e.g. `curl`),
  `flatpak run <app-id>` for flatpaks, or `null` when unknowable (library
  packages, or bulk dnf inventory).
- `install` — the command a user would run to install the application.
- `installed_bytes` / `download_bytes` — on-disk installed size and download
  size respectively; `null` when the manager does not expose them (e.g.
  flatpak/snap catalog-only results, brew catalog formulae).
- `homepage`, `license`, `origin` (repo/remote/tap/publisher), `arch`,
  `maintainer`, `section`, `depends`, `install_date` — metadata exposed when
  the manager provides it; `null` otherwise. `install_date` is RFC3339 when
  derivable (rpm) and the manager-reported string otherwise (pacman).
- `available_version` — newer version for an installed app; only filled by
  the `outdated` command.
- `matched_tokens` — which query tokens matched this app (including
  synonyms); empty means it matched semantically, not lexically.
- `confidence` — relevance in [0, 1]: how much of the query this result
  covers (tokens matched, name/phrase hits, semantic similarity). Only
  filled by `search`; results are sorted by it, best first.
- `errors[].manager` / `errors[].message` — manager-level failures.
- Results are sorted deterministically by name, then manager.

## Supported managers and commands used

| manager | inventory | search | notes |
| --- | --- | --- | --- |
| apt | via dpkg database | `apt-cache search`, `apt-cache policy` | candidate versions, installed flag via dpkg status |
| dpkg | `dpkg-query -W` | — | source of truth for installed debs |
| flatpak | `flatpak list --app` | `flatpak search` | deduplicated by application ID |
| snap | `snap list` | `snap find` | summaries via batched `snap info` |
| brew | `brew list --versions` | `brew search` | descriptions via `brew info --json=v2` |
| pacman | `pacman -Q` | `pacman -Ss` | descriptions via batched `pacman -Qi` |
| dnf | `rpm -qa` | `dnf search` | usage filled only for search hits |

All managers are auto-detected at runtime; only the ones present on the
system are consulted (reflected in `managers_detected`).

Parsing is locale-independent: commands that produce structured output run
with `LC_ALL=C`.

## Semantic search (optional)

`search` can blend lexical scores with multilingual embeddings (bge-m3), so
queries in any language find what they mean — `programa para editar peliculas`
finds a video editor even when no token matches.

Requirements: `llama-server` from llama.cpp and an embedding GGUF, e.g.

```bash
llama-server -m ~/models/bge-m3-Q8_0.gguf --embeddings --port 8080
```

Then build the index once (re-run after installing/removing apps):

```bash
pkgq index [--manager m1,m2]
```

The index is cached at `~/.cache/pkgq/index.json`. While the server is
reachable, `search` blends semantic similarity (0.6) with the lexical score
(0.4) and rescues indexed apps the tokens missed; if the server is down or
there is no index, `search` silently falls back to lexical-only.

Configuration: `PKGQ_EMBED_URL` (default
`http://127.0.0.1:8080/v1/embeddings`) and `PKGQ_EMBED_MODEL` (default
`bge-m3`).

## Build

```bash
cargo build --release
# binary at target/release/pkgq
```

Requires Rust 1.x with the 2021 edition. Tests: `cargo test`.

## Design notes

- **bash-only execution** — a single shell adapter wraps `bash -c`; no direct
  `exec` of managers anywhere else in the codebase.
- **Batched lookups** — per-package queries are grouped into single shell
  invocations (loops or one `grep` over package file lists) so inventories of
  thousands of packages stay fast.
- **Fixture-tested parsers** — every provider parser is unit-tested against
  real captured command output, so managers absent on the development machine
  (pacman, dnf) are still covered.
