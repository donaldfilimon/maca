# maca: production-readiness plan (2026-11-10)

Status: **in progress** (alpha 0.1.0 release cleanup, 2026-11-10).

**Delivered:** Phase 1 (CHANGELOG.md, deny.toml, `/skills/` in .gitignore,
`docs/plans/2026-11-10-versions-casks.md`); Phase 2 item 1 (deny.toml, a
cargo-deny step in check.sh and a cargo-deny job in ci.yml) and item 4 (cask,
version and project-prefix tests); Phase 3 item 1 (named versions on brew,
winget and custom).

**Still open:** Phase 3 items 2-4 (dmg/pkg casks, taps, hermetic builds),
Phase 4 (push, release.yml extensions, install script, tag and self-update
evidence) and Phase 5. Phase 2 items 2-3 (cross-check, unwrap review) and the
Phase 1 README/CLAUDE.md/AGENTS.md reconciliation are not confirmed here and
stay listed as open until verified.

## Decisions (from the user)

- **Scope:** everything, Phases 1-5, including the new backend features.
- **Release:** push `main` and tag `v0.1.0` once all checks pass.

## Where things stand (as of 2026-11-10 at plan time; current baseline is 124 unit and 22 integration tests)

- `./check.sh` passes: 113 unit tests, 16 integration tests, clippy `-D warnings`, release build, doctor.
- `main` is 7 commits ahead of `origin/main` (unpushed). `skills/` is untracked and is tooling, not project code.
- Neither workflow in `.github/workflows` has ever run. No `v*` release exists, so `self-update` has never run against a real release.
- `cargo-audit`, `cargo-deny` and `zig` are installed locally, but nothing in the project uses audit or deny yet.
- Version is `0.1.0`. There is no CHANGELOG and no `deny.toml`.

### Doc drift to fix (the code has moved ahead of the docs)

| Where | Stale claim | Reality |
|---|---|---|
| README "Limits" | "Version pinning is verify-only" | GitHub and Aqua now fetch a named version. Brew, winget and custom refuse it. |
| README "Limits" | "One install per package per data dir" | The latest commits threaded shared cellars across prefixes. Re-verify the real behavior first. |
| README "On the way" and `docs/index.html` | Per-directory versions "planned" | A project file already scopes the prefix (`config` test). |
| README "Lockfiles" | "`sync` cannot fetch an older one" | It can for GitHub and Aqua. |
| `CLAUDE.md` and `AGENTS.md` | Lockfile versions "verify-only" | Same as above. Also missing: casks, `--cask`, project-file prefix. |
| `docs/plans/` | Nothing after the control-plane and completion tranches | Missing a tranche for versions, casks, project prefix and shared cellars. |
| README Commands | `completions` lists "bash, zsh, fish" | Five shells are supported. |

## Phases

Each phase ends with `./check.sh` and `./scripts/cross-check.sh` clean. Each item is its own commit.

### Phase 1: Truth and hygiene (no behavior change)
1. Reconcile the docs in the table above. Read the code for each claim before editing.
2. Add `docs/plans/2026-11-10-versions-casks.md` recording what was delivered and the evidence.
3. Add `skills/` to `.gitignore` (`.work/` is already ignored).
4. Add a CHANGELOG with a `0.1.0` entry.

### Phase 2: Quality gates
1. Add `deny.toml` (license allowlist compatible with Apache-2.0). Add `cargo audit` and `cargo deny check` to CI as required steps and to `check.sh` as optional ones.
2. Run `./scripts/cross-check.sh` and fix any Linux or Windows warnings.
3. Review the `unwrap`/`expect` calls in `src/` (3 in `main.rs`) for reachable panics.
4. Add tests for the newest code paths:
   - cask failure modes: dmg and pkg refused, checksum mismatch, zip-slip cask
   - a version request on a backend that can't serve one
   - the project-file prefix and the cross-prefix refusal, end to end

### Phase 3: Feature gaps from the README roadmap
In this order, smallest and most testable first. Each gets unit and e2e tests (loopback server, as in the existing `e2e_*` tests).
1. **Versions on brew, winget and custom.** Serve a named version where the catalog allows it: versioned formulae (`name@x`), older winget manifests, multiple custom entries per name. Otherwise keep the clear refusal. Update `check_locked_version` and `sync` accordingly.
2. **Cask completeness.** `.dmg` (mount with `hdiutil`, copy out, detach, macOS only) and `.pkg` (payload extraction only, no system install). Optional `--link-apps` to link into `/Applications`, never on by default.
3. **Taps.** A user-supplied formula repository as a source: `maca tap add/list/remove`, with the same checksum rules as other backends. This touches every `BackendKind` dispatch site listed in `AGENTS.md`.
4. **Hermetic builds.** A stronger sandbox: `sandbox-exec` profile on macOS (deny network and writes outside the build dir), a restricted PATH and private HOME/TMP elsewhere.
5. **Services** (`maca services`) are deferred past 1.0 and documented as a non-goal, because they need launchd and systemd integration that can't be verified here.

### Phase 4: Release engineering
1. Push `main`. Watch the first CI run on Ubuntu, macOS and Windows and fix any failure.
2. Extend `release.yml`:
   - a Windows `.zip` asset (`maca-<triple>.zip` plus `.sha256`), so Windows self-update has something to download
   - macOS x86_64 and Linux aarch64 targets
   - an aggregate `SHA256SUMS` file
3. Add an install script (`curl | sh`) that downloads the release archive and verifies its checksum.
4. Tag `v0.1.0` (the workflow checks the tag equals the `Cargo.toml` version). Then run `maca self-update --check` and `maca self-update` against the published release on this Mac, and record the evidence in a plan file.

### Phase 5: Verification on hardware we don't have
- Add a `windows-latest` CI job that runs a real winget package install and the `.zip` self-update against the published release. Until it has run and passed, the docs keep saying "unverified".
- Add a nightly Linux smoke job (brew bottle, GitHub and Aqua installs over live network). It may fail without blocking merges.

## Definition of done for 1.0
- `./check.sh` and `./scripts/cross-check.sh` are clean, and audit and deny are green.
- CI is green on all three OSes. A tagged release exists with verified assets.
- `self-update` has been exercised against that release on macOS (and on Linux in CI).
- The docs match the code, and every remaining limit is listed honestly.
- Nothing unverified on Windows is described as verified.

## Risks
- Phase 3 touches many dispatch sites at once (no shared backend trait). Consider introducing a small trait first if a third change would repeat the edits.
- A first CI run on three OSes will likely surface platform failures that `check.sh` on a Mac can't see.
- Pushing and tagging are public and hard to undo. I'll confirm CI is green before tagging, even though you've approved it.
