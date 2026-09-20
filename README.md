# bash-cli

Inventory and search OS applications across package managers, outputting JSON.
Written in Rust; every external command runs exclusively through `bash -c`.

Primary target: Debian Linux. Other managers are supported opportunistically.

## Usage

```bash
bash-cli list [--manager m1,m2] [--compact]
bash-cli search <query> [--manager m1,m2] [--compact] [--installed-only] [--available-only]
bash-cli outdated [--manager m1,m2] [--compact]
```

- `list` — every application currently installed on the system.
- `search` — the **union** of the installed inventory and each manager's
  catalog results, matched with AND semantics over whitespace-separated
  tokens (case-insensitive, substrings count). Installed apps are found even
  when a manager's remote search does not surface them.
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

## Build

```bash
cargo build --release
# binary at target/release/bash-cli
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
