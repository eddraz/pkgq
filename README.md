# bash-cli

Inventory and search OS applications across package managers, outputting JSON.
Written in Rust; every external command runs exclusively through `bash -c`.

Primary target: Debian Linux. Other managers are supported opportunistically.

## Usage

```bash
bash-cli list [--manager m1,m2] [--compact]
bash-cli search <query> [--manager m1,m2] [--compact] [--installed-only] [--available-only]
```

- `list` — every application currently installed on the system.
- `search` — the **union** of the installed inventory and each manager's
  catalog results, matched with AND semantics over whitespace-separated
  tokens (case-insensitive, substrings count). Installed apps are found even
  when a manager's remote search does not surface them.
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
      "size_bytes": 530432
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
- `size_bytes` — on-disk installed size when the app is installed; download
  size when it is only available; `null` when the manager does not expose it
  (e.g. flatpak/snap catalog-only results, brew catalog formulae).
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
