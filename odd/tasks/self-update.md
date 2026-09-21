# Feature: self-update (`pkgq update`)

Status: implemented on feat/self-update — pending user merge decision

## Goal

`pkgq update` self-updates the binary from GitHub Releases: query the latest
release, compare against the compiled version, download the tar.gz for the
current target over HTTPS (verified TLS), and replace the running binary.

## Decisions (user-confirmed)

- Mechanism: self-update from GitHub Releases (eddraz/pkgq), NOT delegation to
  package managers and NOT check-only.
- No new dependencies: reuse the project's bash shell-out convention plus
  system `curl -fsSL` (TLS verification mandatory; no insecure flag ever).
- Output: JSON, consistent with the frozen v1 style (`"command"` field).

## Design notes

- Asset scheme: `pkgq-$VERSION-$TARGET.tar.gz` (matches release.yml).
- Target triple: cfg-based const mapping (linux/darwin x x86_64/aarch64).
- Latest release: `https://api.github.com/repos/eddraz/pkgq/releases/latest`
  with User-Agent header; pick the matching asset from `assets`.
- Version compare: numeric dot-segments; non-numeric segments fall back to
  inequality.
- Replacement: download+extract to temp dir, copy mode (0755) from current
  exe, write `path.new`, `fs::rename` over the running binary (safe on
  Linux/macOS). Unwritable install dir -> clear JSON error + hint.
- Already current: `updated: false`, no download.

## Tasks

- [x] T1: `update` subcommand in cli.rs + dispatch/main wiring with JSON report
- [x] T2: update module: release query, version compare, target mapping, asset selection
- [x] T3: download, extract, binary replacement with permission handling
- [x] T4: inline unit tests for pure parts (compare, target, asset pick)
- [x] T5: README section for `pkgq update`

## Evidence

- Work-unit commit: feat/self-update — `feat(update): self-update the pkgq binary from GitHub Releases`
- Independent verification (gentle-ai-verify): clean release rebuild 0 warnings; cargo test 92 passed / 0 failed; live `update --compact` -> valid JSON, updated:false, already up to date; security audit clean (no TLS-weakening flags, no new deps, no secrets); diff scope exactly the 4 code surfaces + feature doc.
- Note: download-and-replace path not exercised end-to-end (would mutate the installed binary); validated logically + via helper conventions.
