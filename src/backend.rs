//! Shared package types and backend dispatch.
//!
//! Each backend implements search/info/install via native HTTP APIs.
//! No backend shells out to `brew` or `winget` for catalog operations.
//! Backends are concrete structs sharing method names (`search`, `info`,
//! `plan`, `install`) by convention; there is no trait. Dispatch is a `match`
//! on [`BackendKind`] here and in `main.rs`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Auto,
    Brew,
    Winget,
    Custom,
    Github,
    Aqua,
}

impl BackendKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Brew => "brew",
            Self::Winget => "winget",
            Self::Custom => "custom",
            Self::Github => "github",
            Self::Aqua => "aqua",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "brew" => Self::Brew,
            "winget" => Self::Winget,
            "custom" => Self::Custom,
            "github" => Self::Github,
            "aqua" => Self::Aqua,
            _ => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub description: String,
    pub homepage: String,
    pub backend: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPlan {
    pub name: String,
    pub version: String,
    pub backend: String,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub dry_run: bool,
    pub prefix: std::path::PathBuf,
    /// When set, kegs are stored under this directory instead of the prefix cellar.
    /// Links (bin, opt, share) and the prefix lock remain under `prefix`.
    pub cellar: Option<std::path::PathBuf>,
    /// Package kind recorded in the ledger (app, plugin, cli, ...).
    pub kind: crate::group::Kind,
    /// Compile from source instead of using a prebuilt bottle/installer.
    pub build_from_source: bool,
    /// Skip automatic snapshots before mutating commands.
    pub no_snapshot: bool,
    /// Concurrent install jobs for multi-package commands.
    pub jobs: usize,
    /// Version to install instead of the catalog's current one.
    ///
    /// `None` means the catalog's current version. A backend that can address
    /// an older artifact fetches it; one that cannot refuses, rather than
    /// installing the current version and leaving the caller to notice.
    pub version: Option<String>,
}

impl InstallOptions {
    /// The directory under which kegs for this package are stored.
    /// Resolves `cellar` when set, falling back to `<prefix>/cellar`.
    pub fn keg_root(&self) -> std::path::PathBuf {
        match &self.cellar {
            Some(c) if !c.as_os_str().is_empty() => c.clone(),
            _ => crate::config::cellar_dir(&self.prefix),
        }
    }
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            prefix: std::path::PathBuf::new(),
            cellar: None,
            kind: crate::group::Kind::App,
            build_from_source: false,
            no_snapshot: false,
            jobs: 4,
            version: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InstallOutcome {
    pub name: String,
    pub version: String,
    pub backend: String,
    pub cellar_path: std::path::PathBuf,
    pub linked_bins: Vec<std::path::PathBuf>,
}

/// True when `candidate` is a strictly newer version than `installed`.
///
/// Semver strings compare as semver. Anything else (Homebrew revisions such
/// as `1.8.2_1`, four-part or date versions) is compared component by
/// component: numeric parts numerically, missing parts as zero, then the
/// `_N` revision. When the two cannot be ordered reliably (a numeric part
/// against a word, or equal parts written differently) the answer is
/// "not newer": an upgrade that cannot prove it moves forward must not run,
/// because it could just as well be a downgrade.
pub fn is_newer_version(candidate: &str, installed: &str) -> bool {
    let c = candidate.trim().trim_start_matches('v');
    let i = installed.trim().trim_start_matches('v');
    if let (Ok(a), Ok(b)) = (semver::Version::parse(c), semver::Version::parse(i)) {
        return a > b;
    }
    compare_loose(c, i) == Some(std::cmp::Ordering::Greater)
}

/// True when `actual` is the version `requested` asks for: the same string
/// (ignoring whitespace and a leading `v`), or `requested` is a leading
/// run of `actual`'s dotted parts or its Homebrew revision (`3.12` is met by
/// `3.12.4`, `1.8.2` by `1.8.2_1`). `3.1` is not met by `3.10`, and a
/// pre-release (`1.0-rc1`) never meets `1.0`.
pub fn version_satisfies(requested: &str, actual: &str) -> bool {
    let req = requested.trim().trim_start_matches('v');
    let act = actual.trim().trim_start_matches('v');
    if req.is_empty() {
        return false;
    }
    if req == act {
        return true;
    }
    act.strip_prefix(req)
        .is_some_and(|rest| rest.starts_with(['.', '_']))
}

/// Sort version strings newest first with the same loose ordering `upgrade`
/// uses. Strings that cannot be ordered reliably keep their relative order.
pub fn sort_versions_newest_first(versions: &mut [String]) {
    versions.sort_by(|a, b| {
        let (a, b) = (
            a.trim().trim_start_matches('v'),
            b.trim().trim_start_matches('v'),
        );
        if is_newer_version(a, b) {
            std::cmp::Ordering::Less
        } else if is_newer_version(b, a) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
}

#[derive(Debug, PartialEq)]
enum Tok {
    Num(u64),
    /// A letter run. `pre` when it follows `-` or `~` (`1.0-rc1`): a
    /// pre-release tag, which sorts before the bare version. Otherwise a
    /// post-release suffix attached to a number (`3.5a`, `9.6p1`).
    Word {
        text: String,
        pre: bool,
    },
}

fn tokens(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    let mut last_sep = '.';
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = String::new();
            while let Some(&d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                n.push(d);
                chars.next();
            }
            out.push(Tok::Num(n.parse().unwrap_or(u64::MAX)));
            last_sep = '\0';
        } else if c.is_alphabetic() {
            let mut w = String::new();
            while let Some(&d) = chars.peek().filter(|d| d.is_alphabetic()) {
                w.push(d.to_ascii_lowercase());
                chars.next();
            }
            out.push(Tok::Word {
                text: w,
                pre: matches!(last_sep, '-' | '~'),
            });
            last_sep = '\0';
        } else {
            last_sep = c;
            chars.next();
        }
    }
    out
}

fn compare_loose(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let split_rev = |s: &str| -> (String, u64) {
        match s.rsplit_once('_') {
            Some((m, r)) if !r.is_empty() && r.bytes().all(|c| c.is_ascii_digit()) => {
                (m.to_string(), r.parse().unwrap_or(0))
            }
            _ => (s.to_string(), 0),
        }
    };
    let (ma, ra) = split_rev(a);
    let (mb, rb) = split_rev(b);
    let (ta, tb) = (tokens(&ma), tokens(&mb));
    for i in 0..ta.len().max(tb.len()) {
        let ord = match (ta.get(i), tb.get(i)) {
            (Some(Tok::Num(x)), Some(Tok::Num(y))) => x.cmp(y),
            (Some(Tok::Word { text: x, pre: px }), Some(Tok::Word { text: y, pre: py })) => {
                // A pre-release tag sorts below a post-release suffix.
                py.cmp(px).then_with(|| x.cmp(y))
            }
            // A missing number counts as 0, so 1.0 == 1.0.0.
            (Some(Tok::Num(x)), None) => x.cmp(&0),
            (None, Some(Tok::Num(y))) => 0.cmp(y),
            // Running out against a word: a pre-release tag is older
            // (1.0-rc1 < 1.0), a suffix is newer (3.5 < 3.5a).
            (None, Some(Tok::Word { pre, .. })) => {
                if *pre {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (Some(Tok::Word { pre, .. }), None) => {
                if *pre {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            // A number against a pre-release tag: the number is a later
            // release (1.0.1 > 1.0-rc1). Against a suffix: no reliable order.
            (Some(Tok::Num(_)), Some(Tok::Word { pre: true, .. })) => Ordering::Greater,
            (Some(Tok::Word { pre: true, .. }), Some(Tok::Num(_))) => Ordering::Less,
            _ => return None,
        };
        if ord != Ordering::Equal {
            return Some(ord);
        }
    }
    Some(ra.cmp(&rb))
}

/// The version the package's catalog offers now. Custom-registry mirrors
/// are required here (no silent fallback to the local registry), so an
/// unreachable mirror cannot make an older local entry look current.
pub async fn latest_version(backend: &str, name: &str) -> Result<String> {
    let pkg = match backend {
        "brew" => brew::BrewBackend::new().info(name).await?,
        "winget" => winget::WingetBackend::new().info(name).await?,
        "custom" => {
            let urls = config::Config::load()?.custom_registry_urls;
            CustomBackend::new(custom::Registry::load_merged(&urls).await?).info(name)?
        }
        // Strict lookups: a rate-limited or offline API must not look current.
        "github" => {
            return github::GithubBackend::new()
                .latest_release_version(name)
                .await;
        }
        "aqua" => return aqua::AquaBackend::new().latest_release_version(name).await,
        other => {
            return Err(crate::Error::Unsupported(format!(
                "unknown backend '{other}' in ledger"
            )));
        }
    };
    Ok(pkg.version)
}

/// Resolve which concrete backend handles a request.
pub fn resolve(kind: BackendKind) -> BackendKind {
    match kind {
        BackendKind::Auto => auto_default(),
        other => other,
    }
}

pub fn auto_default() -> BackendKind {
    // Nominal default only: returns Custom. The real orders differ by command:
    // - install: custom, then brew (if `can_install`), then github or aqua when
    //   `insight::suggest_backend` names one, then winget;
    // - search: all five backends run concurrently, shown in `auto_search_order`;
    // - info: custom, brew, winget, then github/aqua.
    BackendKind::Custom
}

/// Ordered fallback list for `--backend auto` search.
/// This is the search and display order only; install order differs (see `auto_default`).
pub fn auto_search_order() -> [BackendKind; 5] {
    [
        BackendKind::Custom,
        BackendKind::Brew,
        BackendKind::Winget,
        BackendKind::Github,
        BackendKind::Aqua,
    ]
}

/// Search every backend in auto order, merging results. Backends run
/// concurrently so one slow or failing catalog does not stall the others.
pub async fn search_all(query: &str, limit: usize) -> Vec<Package> {
    let custom_reg = match custom::Registry::load_configured().await {
        Ok(reg) => Some(reg),
        Err(e) => {
            tracing::warn!("custom registry: {e}");
            None
        }
    };
    let brew = brew::BrewBackend::new();
    let winget = winget::WingetBackend::new();
    let github = github::GithubBackend::new();
    let aqua = aqua::AquaBackend::new();
    let (custom_res, brew_res, winget_res, github_res, aqua_res) = tokio::join!(
        async {
            match custom_reg {
                Some(reg) => CustomBackend::new(reg).search(query, limit),
                None => Ok(vec![]),
            }
        },
        brew.search(query, limit),
        winget.search(query, limit),
        github.search(query, limit),
        aqua.search(query, limit),
    );
    let mut all = Vec::new();
    for result in [custom_res, brew_res, winget_res, github_res, aqua_res] {
        match result {
            Ok(mut pkgs) => all.append(&mut pkgs),
            Err(e) => tracing::warn!("backend search: {e}"),
        }
    }
    all.sort_by(|a, b| a.name.cmp(&b.name).then(a.backend.cmp(&b.backend)));
    all.dedup_by(|a, b| a.name == b.name && a.backend == b.backend);
    insight::rank_packages(query, all)
}

/// Install `name` with a backend name: one of `brew`, `winget`, `custom`, `github`
/// or `aqua`. Any other value (including `auto`) selects auto.
pub async fn install_with_backend(
    backend: &str,
    name: &str,
    opts: &InstallOptions,
) -> Result<InstallOutcome> {
    if let Some(existing) = state::installed_elsewhere(name, &config::cellar_dir(&opts.prefix))? {
        return Err(state::elsewhere_error(name, &existing));
    }
    let kind = BackendKind::from_str_lossy(backend);
    // A requested version under `auto` has nowhere to go: falling through
    // could install a different backend's current version, and the caller
    // would have no way to tell the request was ignored.
    if opts.version.is_some() && kind == BackendKind::Auto {
        let version = opts.version.as_deref().unwrap_or_default();
        return Err(crate::Error::Unsupported(format!(
            "installing {name} at version {version} needs an explicit backend; \
    	         `--backend auto` cannot tell which catalog should serve it"
        )));
    }
    match kind {
        BackendKind::Brew => brew::BrewBackend::new().install(name, opts).await,
        BackendKind::Winget => winget::WingetBackend::new().install(name, opts).await,
        BackendKind::Custom => {
            let reg = custom::Registry::load_configured().await?;
            custom::CustomBackend::new(reg).install(name, opts).await
        }
        BackendKind::Github => github::GithubBackend::new().install(name, opts).await,
        BackendKind::Aqua => aqua::AquaBackend::new().install(name, opts).await,
        BackendKind::Auto => auto_install(name, opts).await,
    }
}

async fn auto_install(name: &str, opts: &InstallOptions) -> Result<InstallOutcome> {
    if let Ok(reg) = custom::Registry::load_configured().await {
        let custom_backend = custom::CustomBackend::new(reg);
        if custom_backend.plan(name).is_ok() {
            return custom_backend.install(name, opts).await;
        }
    }
    if brew::BrewBackend::new().can_install(name).await {
        return brew::BrewBackend::new().install(name, opts).await;
    }
    match insight::suggest_backend(name) {
        Some("github") => return github::GithubBackend::new().install(name, opts).await,
        Some("aqua") => return aqua::AquaBackend::new().install(name, opts).await,
        _ => {}
    }
    winget::WingetBackend::new().install(name, opts).await
}

/// Remove an installed package by name, using the prefix's ledger.
pub fn remove_with_prefix(prefix: &std::path::Path, name: &str) -> Result<()> {
    let ledger = state::Ledger::load()?;
    let record = ledger
        .get(name)
        .cloned()
        .ok_or_else(|| crate::Error::NotFound(format!("package '{name}' is not installed")))?;
    // Only the keg the ledger records is removed; `upgrade` reclaims the
    // previous version itself, so nothing else under `cellar/<name>` is ours
    // to delete.
    remove_keg(prefix, &record.cellar)?;
    if let Some(name_dir) = record.cellar.parent() {
        let _ = std::fs::remove_dir(name_dir); // only if now empty
    }
    let opt_link = prefix.join("opt").join(name);
    if std::fs::read_link(&opt_link).is_ok_and(|t| !t.exists()) {
        let _ = std::fs::remove_file(&opt_link);
    }
    state::remove_record(name)?;
    Ok(())
}

/// Delete one keg directory together with the share and bin links that point
/// into it. Links that point anywhere else are left alone.
pub fn remove_keg(prefix: &std::path::Path, keg: &std::path::Path) -> Result<()> {
    // Never delete outside this prefix's cellar, whatever a ledger record or
    // a caller says.
    if !config::path_under(keg, &config::cellar_dir(prefix)) {
        return Err(crate::Error::Unsupported(format!(
            "{} is not inside {}; refusing to remove it",
            keg.display(),
            config::cellar_dir(prefix).display()
        )));
    }
    let _ = install::unlink_share(keg, prefix);
    let bin_dir = config::bin_dir(prefix);
    #[cfg(not(windows))]
    if bin_dir.exists() {
        for entry in std::fs::read_dir(&bin_dir)?.flatten() {
            let link = entry.path();
            if read_link_target(&link).is_ok_and(|t| t.starts_with(keg)) {
                let _ = std::fs::remove_file(&link);
            }
        }
    }
    // Windows bin entries are copies: remove those still identical to the
    // keg's executables.
    #[cfg(windows)]
    for sub in ["bin", "sbin"] {
        if let Ok(entries) = std::fs::read_dir(keg.join(sub)) {
            for entry in entries.flatten() {
                let copy = bin_dir.join(entry.file_name());
                if install::is_copy_of(&copy, &entry.path()) {
                    let _ = std::fs::remove_file(&copy);
                }
            }
        }
    }
    if keg.is_dir() {
        std::fs::remove_dir_all(keg)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn read_link_target(link: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    std::fs::read_link(link)
}

/// Check phase of a single-package upgrade: network only, takes no lock.
/// `Ok(Err(message))` is a finished outcome (pinned, up to date); `Ok(Ok(plan))`
/// must be passed to [`apply_planned_upgrade`] with the prefix lock held.
pub async fn plan_upgrade(
    name: &str,
) -> Result<std::result::Result<crate::upgrade::Planned, String>> {
    let ledger = state::Ledger::load()?;
    let record = ledger
        .get(name)
        .cloned()
        .ok_or_else(|| crate::Error::NotFound(format!("package '{name}' is not installed")))?;
    if state::is_pinned(&record.name)? {
        return Ok(Err(format!(
            "{} {} is pinned; skipping",
            record.name, record.version
        )));
    }
    let latest = latest_version(&record.backend, &record.name).await?;
    if !is_newer_version(&latest, &record.version) {
        return Ok(Err(format!(
            "{} {} is up to date",
            record.name, record.version
        )));
    }
    Ok(Ok(crate::upgrade::Planned { record, latest }))
}

/// Apply a plan from [`plan_upgrade`]. The caller holds the prefix lock; the
/// plan is re-validated against the ledger and pins as they are now.
pub async fn apply_planned_upgrade(
    prefix: &std::path::Path,
    planned: &crate::upgrade::Planned,
) -> Result<String> {
    let record =
        match crate::upgrade::revalidate(planned, &state::Ledger::load()?, &state::load_pins()?) {
            Ok(r) => r,
            Err(why) => {
                return Ok(format!("{} not upgraded: {why}", planned.record.name));
            }
        };
    let opts = InstallOptions {
        dry_run: false,
        prefix: prefix.to_path_buf(),
        cellar: None,
        kind: record.kind,
        ..Default::default()
    };
    let outcome = install_with_backend(&record.backend, &record.name, &opts).await?;
    // The new keg, its links and its ledger record are in place; reclaim the
    // previous version's keg, but only if what got installed really is
    // newer (the catalog can change between the check and the install).
    if outcome.cellar_path != record.cellar
        && is_newer_version(&outcome.version, &record.version)
        && config::path_under(&record.cellar, &config::cellar_dir(prefix))
    {
        remove_keg(prefix, &record.cellar)?;
    }
    Ok(format!(
        "upgraded {} {} -> {}",
        outcome.name, record.version, outcome.version
    ))
}

use crate::config;
use crate::custom::{self, CustomBackend};
use crate::state;
use crate::{Result, aqua, brew, github, insight, install, winget};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_kind_roundtrip() {
        assert_eq!(BackendKind::from_str_lossy("brew"), BackendKind::Brew);
        assert_eq!(BackendKind::from_str_lossy("WINGET"), BackendKind::Winget);
        assert_eq!(BackendKind::from_str_lossy("github"), BackendKind::Github);
        assert_eq!(BackendKind::from_str_lossy("aqua"), BackendKind::Aqua);
        assert_eq!(BackendKind::from_str_lossy(""), BackendKind::Auto);
        assert_eq!(BackendKind::Brew.as_str(), "brew");
        assert_eq!(BackendKind::Github.as_str(), "github");
        let order = auto_search_order();
        assert_eq!(order[3], BackendKind::Github);
        assert_eq!(order[4], BackendKind::Aqua);
    }

    #[test]
    fn version_ordering_handles_suffixes_and_prereleases() {
        assert!(is_newer_version("3.6", "3.5a"));
        assert!(is_newer_version("3.5a", "3.5"));
        assert!(!is_newer_version("3.5", "3.5a"));
        assert!(is_newer_version("9.7p1", "9.6p1"));
        assert!(is_newer_version("9.6p2", "9.6p1"));
        assert!(is_newer_version("1.1.1w", "1.1.1v"));
        assert!(is_newer_version("1.2.10a", "1.2.9a"));
        assert!(is_newer_version("1.0", "1.0-rc1"));
        assert!(!is_newer_version("1.0-rc1", "1.0"));
        assert!(is_newer_version("1.0.1", "1.0-rc1"));
        assert!(is_newer_version("2.0-beta2", "2.0-beta1"));
    }

    #[test]
    fn version_ordering_handles_brew_revisions_and_refuses_guesses() {
        assert!(is_newer_version("1.8.3", "1.8.2"));
        assert!(!is_newer_version("1.8.2", "1.8.3"));
        assert!(is_newer_version("1.8.2_1", "1.8.2"));
        assert!(
            !is_newer_version("1.8.2", "1.8.2_1"),
            "must not downgrade a revision"
        );
        assert!(is_newer_version("1.2.3.10", "1.2.3.9"));
        assert!(is_newer_version("2024.10.01", "2024.9.30"));
        assert!(is_newer_version("v0.2.0", "0.1.0"));
        assert!(
            !is_newer_version("1.0", "1.0.0"),
            "equal versions written differently"
        );
        assert!(
            !is_newer_version("custom", "0.1.0"),
            "word vs number cannot be ordered"
        );
        assert!(!is_newer_version("1.0.0", "1.0.0"));
    }

    #[test]
    fn resolve_auto_is_custom_first() {
        assert_eq!(resolve(BackendKind::Auto), BackendKind::Custom);
    }

    #[test]
    fn version_satisfies_matches_whole_components_only() {
        assert!(version_satisfies("3.12", "3.12.4"));
        assert!(version_satisfies("v3.12", "3.12"));
        assert!(version_satisfies("1.8.2", "1.8.2_1"));
        assert!(version_satisfies(" 1.0.0 ", "1.0.0"));
        assert!(!version_satisfies("3.1", "3.10"));
        assert!(!version_satisfies("1.0", "1.0-rc1"));
        assert!(!version_satisfies("2", "10.2"));
        assert!(!version_satisfies("", "1.0"));
        assert!(!version_satisfies("1.0.1", "1.0"));
    }
}
