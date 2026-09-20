# Feature: pkg-query-cli (Rust CLI to inventory and search OS applications)

Status: in_progress
Branch: feat/pkg-query-cli (work-unit commits, no push without user decision)

## Goal

Rust CLI (crate `bash-cli`) that inventories every application known to the
package managers present on the system and searches any application (installed
or not), returning structured JSON. Shells out exclusively through `bash`.
Target platform: Debian Linux first; other managers supported opportunistically.

## Decisions (user-confirmed)

- Managers: auto-detect at runtime; consult only present managers
  (apt, dpkg, flatpak, snap, brew, pacman, dnf).
- `usage` field: primary executable invocation derived from the package
  (e.g. `curl --version`); `null` when unknowable.
- Output: always JSON (pretty default, `--compact` flag).

## JSON contract (frozen for v1)

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
      "manager": "dpkg",
      "installed": true,
      "version": "8.14.1-1",
      "description": "command line tool for transferring data with URL syntax",
      "usage": "curl --version",
      "install": "sudo apt install curl"
    }
  ],
  "errors": []
}
```

- `manager` values: apt | dpkg | flatpak | snap | brew | pacman | dnf.
- Installed deb packages are attributed to `dpkg` (same database apt uses);
  not-installed deb search results are attributed to `apt`. Documented in README.
- `usage`: string or null. `errors[].manager` + `message` only when a detected
  manager command fails. Results sorted deterministically (name asc).

## CLI surface

```
bash-cli list [--manager m1,m2] [--compact]
bash-cli search <query> [--manager m1,m2] [--compact] [--installed-only] [--available-only]
```

## Commands per manager

| manager | installed inventory | search available | description | binaries/usage | install cmd |
|---|---|---|---|---|---|
| dpkg | dpkg-query -W | - (apt searches) | dpkg-query -f | dpkg -L pkg \| bin/ | sudo apt install pkg |
| apt | (via dpkg db) | apt-cache search q; apt-cache policy | search line | dpkg -L when installed | sudo apt install pkg |
| flatpak | flatpak list --app | flatpak search q | flatpak info | flatpak run app | flatpak install app |
| snap | snap list | snap find q | snap info | snap run name / binary | sudo snap install name |
| brew | brew list --formula/--cask | brew search q | brew info --json=v2 | brew --prefix bin | brew install name |
| pacman | pacman -Q + -Qi | pacman -Ss q | -Qi/-Si | pacman -Ql when installed | sudo pacman -S name |
| dnf | dnf list installed | dnf search q | dnf info | rpm -ql when installed | sudo dnf install name |

## Tasks

- [x] T1: Scaffold + domain model + bash adapter + Provider trait + detection + CLI skeleton — commit 2b29fcf
- [x] T2: Providers deb (apt/dpkg) + universal (flatpak/snap) with parser tests — commit c4ac331
- [x] T3: External providers (brew/pacman/dnf) with parser tests + README — commit fc7ea89
- [x] T4: Local end-to-end verification (inline; gentle-ai-verify unavailable) — commits faf3bde, 4bab89e

- [x] T5: search unions installed inventory with catalog results (token matching) — in progress

- [x] T6: size_bytes per app (installed size / download size, null unknown) — in progress

- [x] T7: identity/origin/metadata fields + size split — commit pending
- [x] T8: outdated command — in progress

- [x] T9: --installed-only fast path (offline) + apt/dpkg dedup — in progress

## Evidence log

- T1: commit 2b29fcf — 25/25 tests, fmt clean.
- T2: commit c4ac331 — 42/42 tests; live: list 1854 apps, search curl 214 hits.
- T3: commit fc7ea89 — 52/52 tests; live brew search verified.
- T4: commit (this) — 51/51 tests, 0 warnings; 11-check battery 11/11 after
  fixing flatpak "No matches found" message leak; per-manager source parity
  verified (apt 170, flatpak 4, snap 40, brew 12).
- T5: defect reported by user — installed flatpak Drift invisible to search
  because providers only queried remote catalogs. Fix: search = union of
  token-filtered installed inventory + catalog results per provider (new
  helpers query_tokens/app_matches_query/merge_installed_and_catalog);
  dpkg.search now covers local debs absent from apt cache. 55/55 tests;
  live: search drift -> installed true; search video -> Drift included;
  lesson: a failed test build left a stale release binary that masked the fix.
- T6: size_bytes added across providers — dpkg Installed-Size (KiB), apt-cache
  show (installed KiB / download bytes), flatpak list size column (human),
  snap info installed-size or stable-channel download, brew du -sk over
  Cellar/Caskroom, rpm %SIZE (bytes); shared parse_human_size (SI vs IEC).
  Live: 1852/1854 list sizes; curl 530432 B; Drift 63.7 MB.
- Incident: subagent delegation broken this session (gentle-pi SessionWorktreeRegistry
  cached wrong-clone identity; HOME is a git repo). Root cause saved to Engram
  (gentle-pi/delegation-worktree-registry-defect). Fix: relaunch pi from project cwd.
  Workaround used: inline execution via serena MCP file tools; commits stay with parent.
