# maca versions, casks and project-prefix tranche (2026-11-10)

## Objective

Close the gaps the README listed as "on the way": fetch a named version,
install Homebrew casks, scope a prefix to a directory, share kegs between
prefixes, and isolate source builds from the caller's environment.

## Delivered

- **Named versions.** `InstallOptions::version` is honoured by GitHub Releases
  and Aqua (the tag is tried with and without a leading `v`). Brew resolves a
  versioned formula (`name@x`) or its current version when that satisfies the
  request; winget resolves an older manifest directory; custom serves its
  registration history. A request no catalog can serve is refused with
  `Error::Unsupported`, never rounded. Casks refuse a version the catalog does
  not publish. `sync` and groups pass the locked version through.
- **Casks.** `maca install cask:<token>` and `--cask` install zip and tar casks
  from the Homebrew cask API, checksummed. Disk images and installer packages
  are refused before download. Binaries are linked; the `.app` stays in the
  cellar.
- **Project prefix.** A `maca.toml` in a directory or any parent names the
  prefix and optionally the cellar. Relative paths resolve against the file.
- **Shared cellar.** `MACA_CELLAR`, `cellar` in `config.toml` or `maca.toml`
  place kegs outside `<prefix>/cellar`; bin/opt/share stay under the prefix.
- **Hermetic source builds.** A build inherits only the search path, locale and
  temporary directory, and `SOURCE_DATE_EPOCH` is fixed.

## Corrected in the production pass

- An unparseable `maca.toml` was silently ignored. It is now an error
  (`config::project_prefix` / `project_cellar` return `Result`).
- `MACA_CELLAR` was the lowest-priority cellar source in `Ctx::from_cli`,
  contradicting `config.rs`. It is now the highest.

## Still open

- The ledger holds one record per package name for a data dir, so two
  prefixes cannot hold different versions of the same package. Per-directory
  version switching needs a per-prefix ledger.

## Evidence

- `./check.sh` at the release commit: 124 unit tests and 22 integration
  tests, clippy `-D warnings`, release build, doctor.
- Unit tests `an_unparseable_project_file_is_an_error_not_a_fallback` and
  `a_project_file_can_name_a_cellar_relative_to_itself`; integration test
  `a_broken_project_file_stops_the_command_instead_of_using_the_global_prefix`.
