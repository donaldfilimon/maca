//! maca CLI — cross-platform package manager (macOS/Linux, winget catalog on Windows).
//!
//! Examples:
//! ```sh
//! maca search jq
//! maca info jq
//! maca install jq --dry-run
//! maca install jq ripgrep            # concurrent installs
//! maca install jq --build-from-source
//! maca install code --kind app
//! maca app install code
//! maca list --kind cli
//! maca upgrade
//! maca remove jq
//! maca group create dev
//! maca group add dev jq --kind cli
//! maca group install dev
//! maca lock && maca sync
//! maca snapshot create --note "before upgrade"
//! maca snapshot list
//! maca snapshot restore <id>
//! maca self-update
//! maca tui
//! maca doctor
//! ```

use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;
use tokio::sync::Semaphore;

use maca::backend::{self, BackendKind, InstallOptions};
use maca::brew::BrewBackend;
use maca::config;
use maca::custom::{CustomBackend, Registry};
use maca::group::{self, Group, GroupEntry, Kind, LockEntry};
use maca::lock::PrefixLock;
use maca::snapshot;
use maca::state;
use maca::winget::WingetBackend;
use maca::{Error, Result};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(name = "maca", version = maca::VERSION, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Override install prefix (also `MACA_PREFIX`).
    #[arg(long, global = true)]
    prefix: Option<std::path::PathBuf>,

    /// Concurrent install jobs for multi-package commands.
    #[arg(long, global = true, default_value_t = 4)]
    jobs: usize,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Search packages across enabled backends.
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Backend to use; defaults to `default_backend` in config, else auto.
        #[arg(long, value_enum)]
        backend: Option<BackendKind>,
        /// Output JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show package details.
    Info {
        package: String,
        /// Backend to use; defaults to `default_backend` in config, else auto.
        #[arg(long, value_enum)]
        backend: Option<BackendKind>,
        #[arg(long)]
        json: bool,
    },
    /// Install one or more packages (concurrently).
    Install {
        #[arg(required = true)]
        packages: Vec<String>,
        /// Backend to use; defaults to `default_backend` in config, else auto.
        #[arg(long, value_enum)]
        backend: Option<BackendKind>,
        #[arg(long)]
        dry_run: bool,
        /// Compile from source instead of using a bottle/installer.
        #[arg(long)]
        build_from_source: bool,
        /// Package kind recorded in the ledger.
        #[arg(long, value_enum, default_value_t = Kind::App)]
        kind: Kind,
        /// Do not snapshot the prefix before mutating.
        #[arg(long)]
        no_snapshot: bool,
    },
    /// List installed packages.
    List {
        #[arg(long, value_enum)]
        kind: Option<Kind>,
        #[arg(long)]
        json: bool,
    },
    /// Upgrade installed packages to their latest catalog versions.
    Upgrade {
        #[arg(long)]
        package: Vec<String>,
        #[arg(long)]
        no_snapshot: bool,
    },
    /// Remove installed packages.
    Remove {
        #[arg(required = true)]
        packages: Vec<String>,
        #[arg(long)]
        no_snapshot: bool,
    },
    /// Manage the custom registry backend.
    Custom {
        #[command(subcommand)]
        action: CustomCommands,
    },
    /// Health check: prefix, toolchain, network, symlinks.
    Doctor,
    /// Output shell environment exports for eval.
    Shellenv {
        /// Shell type: bash, zsh, fish, csh (default: auto).
        #[arg(long, default_value = "auto")]
        shell: String,
    },
    /// Generate shell completion scripts.
    Completions {
        /// Shell to generate completions for.
        shell: clap_complete::Shell,
    },
    /// Check installed packages for newer catalog versions without upgrading.
    Outdated {
        /// Output JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Clean catalog caches, build logs, temp directories, and prune snapshots.
    Clean {
        /// Also clear index cache and build logs.
        #[arg(long)]
        all: bool,
        /// Number of recent snapshots to keep [default: 5].
        #[arg(long, default_value_t = 5)]
        keep_snapshots: usize,
    },
    /// Interactive terminal UI.
    Tui,
    /// Update the maca binary itself from a release.
    SelfUpdate {
        /// Only report whether an update is available.
        #[arg(long)]
        check: bool,
        /// Install even when the tag is not newer.
        #[arg(long)]
        force: bool,
    },
    /// Prefix snapshots and rollback.
    Snapshot {
        #[command(subcommand)]
        action: SnapshotCommands,
    },
    /// Groups (profiles) of packages.
    Group {
        #[command(subcommand)]
        action: GroupCommands,
    },
    /// Write a lockfile from the installed ledger.
    Lock,
    /// Install exactly what the lockfile pins.
    Sync {
        #[arg(long)]
        dry_run: bool,
        /// Remove installed packages not present in the lockfile.
        #[arg(long)]
        prune: bool,
    },
    /// Manage app-kind packages.
    App {
        #[command(subcommand)]
        action: KindCommands,
    },
    /// Manage plugin-kind packages.
    Plugin {
        #[command(subcommand)]
        action: KindCommands,
    },
    /// Manage cli-kind packages.
    Cli {
        #[command(subcommand)]
        action: KindCommands,
    },
    /// Pin a package to its current version.
    Pin {
        package: String,
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove a pin.
    Unpin { package: String },
    /// Show which installed packages depend on a package.
    Why { package: String },
}

#[derive(Debug, Subcommand)]
enum CustomCommands {
    /// List custom packages.
    List,
    /// Add or replace a custom package entry.
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        version: String,
        /// Artifact URL (tar.gz, zip, or a source archive when build-step is set).
        #[arg(long)]
        url: String,
        /// Expected SHA-256 hex (verified on install).
        #[arg(long)]
        sha256: Option<String>,
        /// Platform key, e.g. macos-arm64. Repeatable; defaults to current host.
        #[arg(long = "platform")]
        platform: Vec<String>,
        /// Artifact kind: tar.gz (default) or binary.
        #[arg(long, default_value = "tar.gz")]
        kind: String,
        /// Build steps run in the extracted source tree. Repeatable.
        #[arg(long = "build-step")]
        build_step: Vec<String>,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "")]
        homepage: String,
    },
    /// Remove a custom package entry.
    Remove { name: String },
}

#[derive(Debug, Subcommand)]
enum SnapshotCommands {
    /// Create a snapshot of the current prefix.
    Create {
        #[arg(long, default_value = "manual")]
        note: String,
        /// Only snapshot these package names.
        #[arg(long)]
        only: Vec<String>,
    },
    /// List snapshots.
    List,
    /// Restore a snapshot.
    Restore { id: String },
    /// Delete a snapshot.
    Delete { id: String },
    /// Prune older snapshots, keeping only the N most recent.
    Prune {
        /// Number of recent snapshots to keep [default: 5].
        #[arg(long, default_value_t = 5)]
        keep: usize,
    },
}

#[derive(Debug, Subcommand)]
enum GroupCommands {
    Create {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    Delete {
        name: String,
    },
    List,
    Show {
        name: String,
    },
    Add {
        name: String,
        package: String,
        /// Backend name: auto, brew, winget, or custom.
        #[arg(long = "backend", value_name = "BACKEND", default_value = "auto")]
        backend_name: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, value_enum, default_value_t = Kind::App)]
        kind: Kind,
        #[arg(long, default_value = "")]
        note: String,
    },
    Remove {
        name: String,
        package: String,
    },
    /// Install every member of a group (concurrently).
    Install {
        name: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Export a group as a lockfile at the prefix.
    Export {
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum KindCommands {
    Install {
        #[arg(required = true)]
        packages: Vec<String>,
        /// Backend to use; defaults to `default_backend` in config, else auto.
        #[arg(long, value_enum)]
        backend: Option<BackendKind>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        build_from_source: bool,
        #[arg(long)]
        no_snapshot: bool,
    },
    Remove {
        #[arg(required = true)]
        packages: Vec<String>,
        #[arg(long)]
        no_snapshot: bool,
    },
    List,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("maca: {e}");
            std::process::exit(1);
        }
    }
}

struct Ctx {
    prefix: std::path::PathBuf,
    backend: BackendKind,
    jobs: usize,
    custom_registry_urls: Vec<String>,
}

impl Ctx {
    fn from_cli(
        prefix: Option<std::path::PathBuf>,
        backend: Option<BackendKind>,
        jobs: usize,
    ) -> Result<Self> {
        let cfg = config::Config::load()?;
        // An explicit --backend (including `auto`) wins; otherwise the
        // config's default_backend; otherwise auto.
        let resolved_backend = backend.unwrap_or_else(|| {
            cfg.default_backend
                .as_deref()
                .map(BackendKind::from_str_lossy)
                .unwrap_or(BackendKind::Auto)
        });
        Ok(Self {
            prefix: prefix.unwrap_or_else(|| cfg.prefix()),
            backend: resolved_backend,
            jobs: jobs.max(1),
            custom_registry_urls: cfg.custom_registry_urls,
        })
    }

    fn opts(
        &self,
        dry_run: bool,
        kind: Kind,
        build_from_source: bool,
        no_snapshot: bool,
    ) -> InstallOptions {
        InstallOptions {
            dry_run,
            prefix: self.prefix.clone(),
            kind,
            build_from_source,
            no_snapshot,
            jobs: self.jobs,
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let backend = match &cli.command {
        Commands::Search { backend, .. } => *backend,
        Commands::Info { backend, .. } => *backend,
        Commands::Install { backend, .. } => *backend,
        Commands::App {
            action: KindCommands::Install { backend, .. },
        }
        | Commands::Plugin {
            action: KindCommands::Install { backend, .. },
        }
        | Commands::Cli {
            action: KindCommands::Install { backend, .. },
        } => *backend,
        _ => None,
    };
    let ctx = Ctx::from_cli(cli.prefix, backend, cli.jobs)?;
    match cli.command {
        Commands::Search {
            query, limit, json, ..
        } => cmd_search(&ctx, &query, limit, json).await,
        Commands::Info { package, json, .. } => cmd_info(&ctx, &package, json).await,
        Commands::Install {
            packages,
            dry_run,
            build_from_source,
            kind,
            no_snapshot,
            ..
        } => {
            let opts = ctx.opts(dry_run, kind, build_from_source, no_snapshot);
            cmd_install(&ctx, &packages, &opts).await
        }
        Commands::List { kind, json } => cmd_list(kind, json),
        Commands::Upgrade {
            package,
            no_snapshot,
        } => cmd_upgrade(&ctx, package, no_snapshot).await,
        Commands::Remove {
            packages,
            no_snapshot,
        } => cmd_remove(&ctx, &packages, no_snapshot),
        Commands::Doctor => cmd_doctor(&ctx).await,
        Commands::Shellenv { shell } => cmd_shellenv(&ctx, &shell),
        Commands::Completions { shell } => cmd_completions(shell),
        Commands::Outdated { json } => cmd_outdated(&ctx, json).await,
        Commands::Clean {
            all,
            keep_snapshots,
        } => cmd_clean(&ctx, all, keep_snapshots),
        // The TUI locks the prefix per action, not for the whole session.
        Commands::Tui => maca::tui::run(ctx.prefix.clone()).await,
        Commands::SelfUpdate { check, force } => cmd_self_update(check, force).await,
        Commands::Snapshot { action } => cmd_snapshot(&ctx, action),
        Commands::Custom { action } => cmd_custom(&ctx, action),
        Commands::Group { action } => cmd_group(&ctx, action).await,
        Commands::Lock => cmd_lock(&ctx),
        Commands::Sync { dry_run, prune } => cmd_sync(&ctx, dry_run, prune).await,
        Commands::App { action } => cmd_kind(&ctx, action, Kind::App).await,
        Commands::Plugin { action } => cmd_kind(&ctx, action, Kind::Plugin).await,
        Commands::Cli { action } => cmd_kind(&ctx, action, Kind::Cli).await,
        Commands::Pin { package, version } => cmd_pin(&ctx, package, version),
        Commands::Unpin { package } => cmd_unpin(&ctx, package),
        Commands::Why { package } => cmd_why(&ctx, &package).await,
    }
}

async fn cmd_search(ctx: &Ctx, query: &str, limit: usize, json: bool) -> Result<()> {
    let all = if ctx.backend == BackendKind::Auto {
        backend::search_all(query, limit).await
    } else {
        match ctx.backend {
            BackendKind::Brew => BrewBackend::new().search(query, limit).await?,
            BackendKind::Winget => WingetBackend::new().search(query, limit).await?,
            BackendKind::Custom => {
                let reg = Registry::load_merged(&ctx.custom_registry_urls).await?;
                CustomBackend::new(reg).search(query, limit)?
            }
            BackendKind::Auto => unreachable!(),
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    if all.is_empty() {
        println!("no packages matching '{query}'");
        return Ok(());
    }
    println!(
        "{:<24} {:<12} {:<8} DESCRIPTION",
        "NAME", "VERSION", "BACKEND"
    );
    for p in &all {
        let desc: String = p.description.chars().take(48).collect();
        println!("{:<24} {:<12} {:<8} {}", p.name, p.version, p.backend, desc);
    }
    Ok(())
}

async fn cmd_info(ctx: &Ctx, package: &str, json: bool) -> Result<()> {
    let p = info_one(ctx, package).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&p)?);
        return Ok(());
    }
    println!("name:        {}", p.name);
    println!("version:     {}", p.version);
    println!("backend:     {}", p.backend);
    println!("homepage:    {}", p.homepage);
    println!("description: {}", p.description);
    if !p.tags.is_empty() {
        println!("tags:        {}", p.tags.join(", "));
    }
    let ledger = state::Ledger::load().ok();
    let pins = pinned_versions().unwrap_or_default();
    if let Some(r) = ledger.as_ref().and_then(|l| l.get(&p.name)) {
        let is_pinned = pins.contains_key(&p.name);
        println!(
            "installed:   {} (kind: {}, pinned: {})",
            r.version,
            r.kind.as_str(),
            if is_pinned { "yes" } else { "no" }
        );
        println!("cellar:      {}", r.cellar.display());
    } else {
        println!("installed:   no");
    }
    Ok(())
}

async fn info_one(ctx: &Ctx, package: &str) -> Result<maca::backend::Package> {
    match ctx.backend {
        BackendKind::Brew => BrewBackend::new().info(package).await,
        BackendKind::Winget => WingetBackend::new().info(package).await,
        BackendKind::Custom => {
            let reg = Registry::load_merged(&ctx.custom_registry_urls).await?;
            CustomBackend::new(reg).info(package)
        }
        BackendKind::Auto => {
            let reg = Registry::load_merged(&ctx.custom_registry_urls).await?;
            let custom = CustomBackend::new(reg);
            if let Ok(p) = custom.info(package) {
                return Ok(p);
            }
            match BrewBackend::new().info(package).await {
                Ok(p) => Ok(p),
                Err(brew_err) => match WingetBackend::new().info(package).await {
                    Ok(p) => Ok(p),
                    Err(winget_err) => Err(Error::NotFound(format!(
                        "'{package}' not found in any backend (custom: no match, brew: {brew_err}, winget: {winget_err})"
                    ))),
                },
            }
        }
    }
}

/// Install multiple packages concurrently under a single prefix lock.
async fn cmd_install(ctx: &Ctx, packages: &[String], opts: &InstallOptions) -> Result<()> {
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
    let snapshot = maybe_snapshot(ctx, opts, "before install", &[])?;
    let result = install_many(ctx, packages, opts).await;
    if let Err(e) = &result
        && let Some(s) = &snapshot
    {
        match snapshot::restore(&s.id, &ctx.prefix) {
            Ok(_) => eprintln!(
                "maca: install failed ({e}); rolled back to snapshot {}",
                s.id
            ),
            Err(r) => eprintln!("maca: install failed ({e}); rollback also failed: {r}"),
        }
    }
    match result {
        Ok(upgraded) => {
            if upgraded > 0 {
                if opts.dry_run {
                    println!("[dry-run] would install {upgraded} package(s)");
                } else {
                    println!("installed {upgraded} package(s)");
                }
            }
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

async fn install_many(ctx: &Ctx, packages: &[String], opts: &InstallOptions) -> Result<usize> {
    let semaphore = Arc::new(Semaphore::new(ctx.jobs));
    let mut handles = Vec::new();
    // The same name twice would race itself.
    let mut seen = std::collections::HashSet::new();
    let packages: Vec<&String> = packages
        .iter()
        .filter(|p| seen.insert(p.to_ascii_lowercase()))
        .collect();
    for name in packages {
        let ctx_prefix = ctx.prefix.clone();
        let backend = ctx.backend;
        let name = name.clone();
        let sem = semaphore.clone();
        let kind = opts.kind;
        let bfs = opts.build_from_source;
        let dry = opts.dry_run;
        let jobs = ctx.jobs;
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let opts = InstallOptions {
                dry_run: dry,
                prefix: ctx_prefix.clone(),
                kind,
                build_from_source: bfs,
                no_snapshot: true,
                jobs,
            };
            // One entry point for every install, so its checks (such as
            // "already installed in another prefix") always run.
            backend::install_with_backend(backend.as_str(), &name, &opts).await
        }));
    }
    let mut ok = 0usize;
    let mut first_err: Option<Error> = None;
    for handle in handles {
        match handle.await {
            Ok(Ok(outcome)) => {
                if !opts.dry_run {
                    println!(
                        "installed {} {} via {} -> {}",
                        outcome.name,
                        outcome.version,
                        outcome.backend,
                        outcome.cellar_path.display()
                    );
                    for bin in &outcome.linked_bins {
                        println!("  linked {}", bin.display());
                    }
                } else {
                    println!(
                        "[dry-run] {} {} via {}",
                        outcome.name, outcome.version, outcome.backend
                    );
                }
                ok += 1;
            }
            Ok(Err(e)) => {
                eprintln!("error: {e}");
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
            Err(join) => {
                eprintln!("error: task panicked: {join}");
                if first_err.is_none() {
                    first_err = Some(Error::Other(anyhow::anyhow!("install task failed: {join}")));
                }
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(ok),
    }
}

fn maybe_snapshot(
    ctx: &Ctx,
    opts: &InstallOptions,
    reason: &str,
    only: &[String],
) -> Result<Option<snapshot::SnapshotManifest>> {
    if opts.dry_run || opts.no_snapshot {
        return Ok(None);
    }
    let m = snapshot::create(&ctx.prefix, reason, only)?;
    println!("snapshot {} created", m.id);
    Ok(Some(m))
}

fn cmd_list(kind: Option<Kind>, json: bool) -> Result<()> {
    let ledger = state::Ledger::load()?;
    let rows: Vec<&state::Record> = ledger
        .packages
        .iter()
        .filter(|r| kind.map(|k| r.kind == k).unwrap_or(true))
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no packages installed");
        return Ok(());
    }
    let pins = pinned_versions().unwrap_or_default();
    println!(
        "{:<24} {:<12} {:<8} {:<8} {:<8} CELLAR",
        "NAME", "VERSION", "BACKEND", "KIND", "PINNED"
    );
    for r in rows {
        let pinned = if pins.contains_key(&r.name) {
            "yes"
        } else {
            "-"
        };
        println!(
            "{:<24} {:<12} {:<8} {:<8} {:<8} {}",
            r.name,
            r.version,
            r.backend,
            r.kind.as_str(),
            pinned,
            r.cellar.display()
        );
    }
    Ok(())
}

/// Real environment for [`maca::upgrade::run`]: the shared ledger and pins,
/// the live catalogs, and snapshots of this prefix.
struct LiveUpgradeEnv<'a> {
    ctx: &'a Ctx,
    no_snapshot: bool,
}

impl maca::upgrade::UpgradeEnv for LiveUpgradeEnv<'_> {
    fn ledger(&self) -> Result<state::Ledger> {
        state::Ledger::load()
    }
    fn pins(&self) -> Result<std::collections::BTreeMap<String, String>> {
        state::load_pins()
    }
    async fn latest(&self, record: &state::Record) -> Result<String> {
        backend::latest_version(&record.backend, &record.name).await
    }
    fn snapshot(&self) -> Result<Option<String>> {
        if self.no_snapshot {
            return Ok(None);
        }
        Ok(Some(
            snapshot::create(&self.ctx.prefix, "before upgrade", &[])?.id,
        ))
    }
    fn restore(&self, snapshot_id: &str) -> Result<()> {
        snapshot::restore(snapshot_id, &self.ctx.prefix).map(|_| ())
    }
    async fn apply(&self, record: &state::Record) -> Result<()> {
        let opts = InstallOptions {
            dry_run: false,
            prefix: self.ctx.prefix.clone(),
            kind: record.kind,
            build_from_source: false,
            no_snapshot: true,
            jobs: self.ctx.jobs,
        };
        let out = backend::install_with_backend(&record.backend, &record.name, &opts).await?;
        // Reclaim the previous version's keg now that the new one is linked
        // and recorded.
        if out.cellar_path != record.cellar
            && record
                .cellar
                .starts_with(config::cellar_dir(&self.ctx.prefix))
            && let Err(e) = backend::remove_keg(&self.ctx.prefix, &record.cellar)
        {
            eprintln!("warning: could not remove old {}: {e}", record.name);
        }
        Ok(())
    }
}

/// Version checks run with no lock held; the prefix lock is taken only around
/// the state change, after re-validating the plan (see `maca::upgrade`).
async fn cmd_upgrade(ctx: &Ctx, only: Vec<String>, no_snapshot: bool) -> Result<()> {
    let env = LiveUpgradeEnv { ctx, no_snapshot };
    maca::upgrade::run(&env, &ctx.prefix, &only)
        .await
        .map(|_| ())
}

fn is_newer(candidate: &str, installed: &str) -> bool {
    backend::is_newer_version(candidate, installed)
}

async fn latest_version(_ctx: &Ctx, backend: &str, name: &str) -> Result<Option<String>> {
    backend::latest_version(backend, name).await.map(Some)
}

fn cmd_remove(ctx: &Ctx, packages: &[String], no_snapshot: bool) -> Result<()> {
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
    let snapshot = if no_snapshot {
        None
    } else {
        Some(snapshot::create(&ctx.prefix, "before remove", &[])?)
    };
    let mut failed = Vec::new();
    for name in packages {
        match backend::remove_with_prefix(&ctx.prefix, name) {
            Ok(()) => println!("removed {name}"),
            Err(e) => {
                eprintln!("error: {e}");
                failed.push(format!("{name}: {e}"));
            }
        }
    }
    if !failed.is_empty() {
        if let Some(s) = &snapshot {
            match snapshot::restore(&s.id, &ctx.prefix) {
                Ok(_) => eprintln!("maca: rolled back to snapshot {}", s.id),
                Err(r) => eprintln!("maca: rollback failed: {r}"),
            }
        }
        return Err(Error::Other(anyhow::anyhow!(
            "remove failed for: {}",
            failed.join(", ")
        )));
    }
    Ok(())
}

async fn cmd_doctor(ctx: &Ctx) -> Result<()> {
    config::ensure_dirs(&ctx.prefix)?;
    println!("maca doctor");
    println!("  version: {}", maca::VERSION);
    println!("  data dir: {}", config::data_dir().display());
    println!("  prefix: {}", ctx.prefix.display());
    println!(
        "  host: {} / {}",
        if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else {
            "unknown"
        },
        std::env::consts::ARCH
    );
    println!("  backend: {}", ctx.backend.as_str());
    println!("  jobs: {}", ctx.jobs);

    let mut passes = 0usize;
    let mut warnings = 0usize;
    let mut failures = 0usize;

    // 1. Prefix writability check
    let test_file = ctx.prefix.join(".maca-doctor-probe");
    match std::fs::write(&test_file, b"probe") {
        Ok(_) => {
            let _ = std::fs::remove_file(&test_file);
            println!("  [OK] prefix is writable");
            passes += 1;
        }
        Err(e) => {
            println!("  [FAIL] prefix is not writable: {e}");
            failures += 1;
        }
    }

    // 2. PATH check
    let bin_dir = config::bin_dir(&ctx.prefix);
    let path_var = std::env::var("PATH").unwrap_or_default();
    let in_path = std::env::split_paths(&path_var).any(|p| p == bin_dir);
    if in_path {
        println!("  [OK] prefix bin is in PATH");
        passes += 1;
    } else {
        println!(
            "  [WARN] {} is not in PATH (run 'eval \"$(maca shellenv)\"' to add it)",
            bin_dir.display()
        );
        warnings += 1;
    }

    // 3. Required host toolchain check
    #[cfg(target_os = "macos")]
    {
        for tool in ["install_name_tool", "codesign", "otool"] {
            if has_tool(tool) {
                passes += 1;
            } else {
                println!(
                    "  [FAIL] required tool '{tool}' not found (install Xcode Command Line Tools: xcode-select --install)"
                );
                failures += 1;
            }
        }
    }

    // 4. Build tools check (source builds)
    for tool in ["make", "cc"] {
        if has_tool(tool) {
            passes += 1;
        } else {
            println!("  [WARN] build tool '{tool}' not found on PATH (needed for source builds)");
            warnings += 1;
        }
    }

    // 5. Catalog API Connectivity
    let client = maca::install::http_client()?;
    let endpoints = [
        (
            "brew formula API",
            "https://formulae.brew.sh/api/formula/jq.json",
        ),
        (
            "winget catalog API",
            "https://api.winget.run/v2/packages?query=git&take=1",
        ),
    ];
    for (name, url) in endpoints {
        match client.head(url).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() < 500 => {
                passes += 1;
            }
            Ok(resp) => {
                println!("  [WARN] {name} returned HTTP {}", resp.status().as_u16());
                warnings += 1;
            }
            Err(e) => {
                println!("  [WARN] {name} unreachable: {e}");
                warnings += 1;
            }
        }
    }

    // 6. Symlink integrity in prefix bin
    if bin_dir.exists() {
        let mut broken = 0;
        if let Ok(entries) = std::fs::read_dir(&bin_dir) {
            for entry in entries.flatten() {
                let link = entry.path();
                if let Ok(target) = std::fs::read_link(&link)
                    && !target.exists()
                    && !link.exists()
                {
                    broken += 1;
                }
            }
        }
        if broken == 0 {
            passes += 1;
        } else {
            println!(
                "  [WARN] found {broken} broken symlink(s) in {}",
                bin_dir.display()
            );
            warnings += 1;
        }
    }

    println!(
        "  custom: {} entries",
        Registry::load_local()?.packages.len()
    );
    println!(
        "  installed: {} packages",
        state::Ledger::load()?.packages.len()
    );
    println!("  groups: {}", Group::list()?.len());
    println!("  snapshots: {}", snapshot::list()?.len());
    println!("  lock: {}", group::lock_path(&ctx.prefix).display());
    println!(
        "  update repo: {}",
        maca::selfupdate::repo_from_config(&config::Config::load().unwrap_or_default())
    );

    println!("doctor summary: {passes} passed, {warnings} warning(s), {failures} failure(s)");
    if failures > 0 {
        return Err(Error::Other(anyhow::anyhow!(
            "doctor detected {failures} issue(s) needing attention"
        )));
    }
    Ok(())
}

fn has_tool(tool: &str) -> bool {
    let path = match std::env::var("PATH") {
        Ok(p) => p,
        Err(_) => return false,
    };
    for dir in std::env::split_paths(&path) {
        if dir.join(tool).exists() {
            return true;
        }
        #[cfg(windows)]
        {
            if dir.join(format!("{tool}.exe")).exists() {
                return true;
            }
        }
    }
    false
}

fn cmd_shellenv(ctx: &Ctx, shell: &str) -> Result<()> {
    let prefix = ctx.prefix.to_string_lossy();
    let shell_type = if shell == "auto" {
        std::env::var("SHELL")
            .ok()
            .and_then(|s| s.split('/').next_back().map(String::from))
            .unwrap_or_else(|| "bash".into())
    } else {
        shell.to_lowercase()
    };

    match shell_type.as_str() {
        "fish" => {
            println!("set -gx MACA_PREFIX \"{prefix}\";");
            println!("fish_add_path -g \"{prefix}/bin\";");
            println!(
                "if test -d \"{prefix}/share/man\"; set -gx MANPATH \"{prefix}/share/man\" $MANPATH; end;"
            );
            println!(
                "if test -d \"{prefix}/share/info\"; set -gx INFOPATH \"{prefix}/share/info\" $INFOPATH; end;"
            );
        }
        "csh" | "tcsh" => {
            println!("setenv MACA_PREFIX \"{prefix}\";");
            println!("setenv PATH \"{prefix}/bin:$PATH\";");
            println!("setenv MANPATH \"{prefix}/share/man:$MANPATH\";");
            println!("setenv INFOPATH \"{prefix}/share/info:$INFOPATH\";");
        }
        _ => {
            // sh, bash, zsh, ksh, default
            println!("export MACA_PREFIX=\"{prefix}\";");
            println!("export PATH=\"{prefix}/bin:$PATH\";");
            println!("export MANPATH=\"{prefix}/share/man${{MANPATH+:$MANPATH}}\";");
            println!("export INFOPATH=\"{prefix}/share/info${{INFOPATH+:$INFOPATH}}\";");
        }
    }
    Ok(())
}

fn cmd_completions(shell: clap_complete::Shell) -> Result<()> {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "maca", &mut std::io::stdout());
    Ok(())
}

#[derive(Debug, Serialize)]
struct OutdatedPackage {
    name: String,
    installed: String,
    latest: String,
    backend: String,
    pinned: bool,
}

async fn cmd_outdated(ctx: &Ctx, json: bool) -> Result<()> {
    let ledger = state::Ledger::load()?;
    if ledger.packages.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("no packages installed");
        }
        return Ok(());
    }
    let pins = pinned_versions().unwrap_or_default();
    let mut outdated = Vec::new();
    for record in &ledger.packages {
        let is_pinned = pins.contains_key(&record.name);
        let latest = match latest_version(ctx, &record.backend, &record.name).await {
            Ok(Some(v)) => v,
            _ => continue,
        };
        if is_newer(&latest, &record.version) {
            outdated.push(OutdatedPackage {
                name: record.name.clone(),
                installed: record.version.clone(),
                latest,
                backend: record.backend.clone(),
                pinned: is_pinned,
            });
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&outdated)?);
        return Ok(());
    }
    if outdated.is_empty() {
        println!("all packages are up to date");
        return Ok(());
    }
    println!(
        "{:<24} {:<12} {:<12} {:<8} PINNED",
        "NAME", "INSTALLED", "LATEST", "BACKEND"
    );
    for p in &outdated {
        let pin_str = if p.pinned { "yes" } else { "no" };
        println!(
            "{:<24} {:<12} {:<12} {:<8} {}",
            p.name, p.installed, p.latest, p.backend, pin_str
        );
    }
    Ok(())
}

fn cmd_clean(ctx: &Ctx, all: bool, keep_snapshots: usize) -> Result<()> {
    // Held so a concurrent install's staging dir or the snapshot a rollback
    // is reading cannot be swept from under it.
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
    let mut reclaimed = 0u64;

    // 1. Clean brew-index cache if all
    let cache_dir = config::data_dir().join("cache");
    if cache_dir.exists()
        && all
        && let Ok(entries) = std::fs::read_dir(&cache_dir)
    {
        for entry in entries.flatten() {
            if let Ok(m) = entry.metadata() {
                reclaimed += m.len();
            }
            let _ = std::fs::remove_file(entry.path());
        }
        println!("cleaned catalog cache");
    }

    // 2. Clean build logs (always; they only matter right after a failure)
    let logs_dir = config::data_dir().join("logs");
    if logs_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&logs_dir)
    {
        for entry in entries.flatten() {
            if let Ok(m) = entry.metadata() {
                reclaimed += m.len();
            }
            let _ = std::fs::remove_file(entry.path());
        }
        println!("cleaned build logs");
    }

    // 3. Clean temporary directories in cellar (__tmp, .tmp-*)
    let cellar = config::cellar_dir(&ctx.prefix);
    if cellar.exists()
        && let Ok(entries) = std::fs::read_dir(&cellar)
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("__tmp") || name.starts_with(".tmp-") {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    // 4. Prune snapshots
    let pruned = snapshot::prune(keep_snapshots)?;
    if pruned > 0 {
        println!("pruned {pruned} snapshot(s) (keeping {keep_snapshots} latest)");
    }

    let human_size = if reclaimed > 1024 * 1024 {
        format!("{:.1} MiB", reclaimed as f64 / (1024.0 * 1024.0))
    } else if reclaimed > 1024 {
        format!("{:.1} KiB", reclaimed as f64 / 1024.0)
    } else {
        format!("{reclaimed} B")
    };
    println!("clean complete: {human_size} reclaimed");
    Ok(())
}

fn cmd_custom(ctx: &Ctx, action: CustomCommands) -> Result<()> {
    // Registry edits are read-modify-write; serialize them with installs.
    let _lock = match action {
        CustomCommands::List => None,
        // The registry lives in the shared data dir: other prefixes edit it too.
        _ => Some((PrefixLock::acquire(&ctx.prefix)?, state::state_lock()?)),
    };
    match action {
        CustomCommands::List => {
            let reg = Registry::load_local()?;
            if reg.packages.is_empty() {
                println!("no custom packages");
                println!("registry: {}", config::custom_registry_path().display());
                return Ok(());
            }
            println!("{:<24} {:<12} {:<24} BUILD", "NAME", "VERSION", "ARTIFACTS");
            for p in &reg.packages {
                let build = if p.build_steps.is_empty() {
                    "-".to_string()
                } else {
                    format!("{} steps", p.build_steps.len())
                };
                println!(
                    "{:<24} {:<12} {:<24} {}",
                    p.name,
                    p.version,
                    p.artifacts
                        .keys()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    build
                );
            }
            Ok(())
        }
        CustomCommands::Add {
            name,
            version,
            url,
            sha256,
            platform,
            kind,
            build_step,
            description,
            homepage,
        } => {
            if !maca::install::valid_component(&name) || !maca::install::valid_component(&version) {
                return Err(Error::Config(format!(
                    "name {name:?} and version {version:?} must be plain names (no path separators, `.` or `..`)"
                )));
            }
            let artifact_kind = match kind.to_ascii_lowercase().as_str() {
                "binary" => maca::custom::ArtifactKind::Binary,
                "tar.gz" | "targz" | "tar-gz" => maca::custom::ArtifactKind::TarGz,
                other => {
                    return Err(Error::Config(format!(
                        "unknown artifact kind '{other}' (expected tar.gz or binary)"
                    )));
                }
            };
            let platforms = if platform.is_empty() {
                vec![host_platform_key()]
            } else {
                platform
            };
            let mut artifacts: BTreeMap<String, maca::custom::Artifact> = BTreeMap::new();
            for p in platforms {
                artifacts.insert(
                    p.clone(),
                    maca::custom::Artifact {
                        url: url.clone(),
                        sha256: sha256.clone(),
                        kind: artifact_kind,
                    },
                );
            }
            let mut reg = Registry::load_local()?;
            reg.upsert(maca::custom::Entry {
                name: name.clone(),
                version,
                description,
                homepage,
                artifacts,
                build_steps: build_step,
            });
            reg.save_local()?;
            println!(
                "registered custom package '{name}' -> {}",
                config::custom_registry_path().display()
            );
            Ok(())
        }
        CustomCommands::Remove { name } => {
            let mut reg = Registry::load_local()?;
            if reg.remove(&name) {
                reg.save_local()?;
                println!("removed custom package '{name}'");
            } else {
                return Err(Error::NotFound(format!(
                    "custom package '{name}' not found"
                )));
            }
            Ok(())
        }
    }
}

fn host_platform_key() -> String {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "unknown"
    };
    format!("{os}-{arch}")
}

async fn cmd_self_update(check: bool, force: bool) -> Result<()> {
    let cfg = config::Config::load()?;
    let repo = maca::selfupdate::repo_from_config(&cfg);
    let Some(release) = maca::selfupdate::fetch_latest(&repo).await? else {
        println!("no releases published for {repo}");
        return Ok(());
    };
    let tag = release.tag_name.trim_start_matches('v').to_string();
    if tag.is_empty() {
        println!("no releases published for {repo}");
        return Ok(());
    }
    if check {
        if maca::selfupdate::is_newer(&tag, maca::VERSION) {
            println!("update available: {} -> {tag}", maca::VERSION);
        } else {
            println!("up to date ({})", maca::VERSION);
        }
        return Ok(());
    }
    match maca::selfupdate::run(&repo, maca::VERSION, force).await? {
        s if s.starts_with("already up to date") => println!("{s}"),
        tag => println!("updated to {tag}"),
    }
    Ok(())
}

fn cmd_snapshot(ctx: &Ctx, action: SnapshotCommands) -> Result<()> {
    // Listing reads only; everything else changes snapshots or the prefix.
    let _lock = match action {
        SnapshotCommands::List => None,
        _ => Some(PrefixLock::acquire(&ctx.prefix)?),
    };
    match action {
        SnapshotCommands::Create { note, only } => {
            let m = snapshot::create(&ctx.prefix, &note, &only)?;
            println!("snapshot {} created ({} packages)", m.id, m.packages.len());
        }
        SnapshotCommands::List => {
            let snapshots = snapshot::list()?;
            if snapshots.is_empty() {
                println!("no snapshots");
                return Ok(());
            }
            println!("{:<28} {:<8} REASON", "ID", "PKGS");
            for s in snapshots {
                println!("{:<28} {:<8} {}", s.id, s.packages.len(), s.reason);
            }
        }
        SnapshotCommands::Restore { id } => {
            let m = snapshot::restore(&id, &ctx.prefix)?;
            println!("restored snapshot {} ({} packages)", m.id, m.packages.len());
        }
        SnapshotCommands::Delete { id } => {
            snapshot::delete(&id)?;
            println!("deleted snapshot {id}");
        }
        SnapshotCommands::Prune { keep } => {
            let pruned = snapshot::prune(keep)?;
            println!("pruned {pruned} snapshot(s) (keeping {keep} latest)");
        }
    }
    Ok(())
}

async fn cmd_group(ctx: &Ctx, action: GroupCommands) -> Result<()> {
    // Group edits are read-modify-write. `Install` takes the (non-reentrant)
    // lock itself, and the read-only arms need none.
    let _lock = match action {
        GroupCommands::Create { .. }
        | GroupCommands::Delete { .. }
        | GroupCommands::Add { .. }
        | GroupCommands::Remove { .. }
        | GroupCommands::Export { .. } => Some((
            PrefixLock::acquire(&ctx.prefix)?,
            // Groups live in the shared data dir, so other prefixes edit them too.
            state::state_lock()?,
        )),
        _ => None,
    };
    match action {
        GroupCommands::Create { name, description } => {
            let group = Group {
                name: name.clone(),
                description,
                packages: vec![],
            };
            group.save()?;
            println!("created group '{name}'");
        }
        GroupCommands::Delete { name } => {
            Group::delete(&name)?;
            println!("deleted group '{name}'");
        }
        GroupCommands::List => {
            let groups = Group::list()?;
            if groups.is_empty() {
                println!("no groups");
                return Ok(());
            }
            println!("{:<20} {:<8} DESCRIPTION", "NAME", "PKGS");
            for g in groups {
                println!("{:<20} {:<8} {}", g.name, g.packages.len(), g.description);
            }
        }
        GroupCommands::Show { name } => {
            let g = Group::load(&name)?;
            println!("group: {}", g.name);
            println!("description: {}", g.description);
            for p in &g.packages {
                println!(
                    "  {:<24} {:<8} {:<8} {}",
                    p.name,
                    p.kind.as_str(),
                    p.backend.as_deref().unwrap_or("auto"),
                    p.version.as_deref().unwrap_or("(latest)")
                );
            }
        }
        GroupCommands::Add {
            name,
            package,
            backend_name,
            version,
            kind,
            note,
        } => {
            let mut g = Group::load(&name)?;
            g.upsert(GroupEntry {
                name: package.clone(),
                backend: Some(backend_name),
                version,
                kind,
                note: if note.is_empty() { None } else { Some(note) },
            });
            g.save()?;
            println!("added {package} to group '{name}'");
        }
        GroupCommands::Remove { name, package } => {
            let mut g = Group::load(&name)?;
            if g.remove(&package) {
                g.save()?;
                println!("removed {package} from group '{name}'");
            } else {
                return Err(Error::NotFound(format!(
                    "'{package}' is not in group '{name}'"
                )));
            }
        }
        GroupCommands::Install { name, dry_run } => {
            // Read the group only once the lock is held, so an edit made
            // while this waited is seen.
            let _lock = PrefixLock::acquire(&ctx.prefix)?;
            let g = Group::load(&name)?;
            let snapshot = if dry_run {
                None
            } else {
                Some(snapshot::create(&ctx.prefix, "before group install", &[])?)
            };
            let semaphore = Arc::new(Semaphore::new(ctx.jobs));
            let mut handles = Vec::new();
            for entry in &g.packages {
                let sem = semaphore.clone();
                let prefix = ctx.prefix.clone();
                let entry = entry.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    let opts = InstallOptions {
                        dry_run,
                        prefix,
                        kind: entry.kind,
                        build_from_source: false,
                        no_snapshot: true,
                        jobs: 1,
                    };
                    let out = backend::install_with_backend(
                        entry.backend.as_deref().unwrap_or("auto"),
                        &entry.name,
                        &opts,
                    )
                    .await?;
                    check_locked_version(&out, entry.version.as_deref())?;
                    Ok::<_, Error>(out)
                }));
            }
            let mut ok = 0;
            let mut failed = 0;
            for handle in handles {
                match handle.await {
                    Ok(Ok(out)) => {
                        println!("installed {} {}", out.name, out.version);
                        ok += 1;
                    }
                    Ok(Err(e)) => {
                        eprintln!("error: {e}");
                        failed += 1;
                    }
                    Err(j) => {
                        eprintln!("error: task panicked: {j}");
                        failed += 1;
                    }
                }
            }
            println!("group '{name}': {ok} installed, {failed} failed");
            if failed > 0 {
                if let Some(snap) = &snapshot {
                    match snapshot::restore(&snap.id, &ctx.prefix) {
                        Ok(_) => eprintln!("maca: rolled back to snapshot {}", snap.id),
                        Err(r) => eprintln!("maca: rollback failed: {r}"),
                    }
                }
                return Err(Error::Other(anyhow::anyhow!(
                    "group '{name}': {failed} package(s) failed"
                )));
            }
        }
        GroupCommands::Export { name } => {
            let g = Group::load(&name)?;
            let lock = group::Lockfile {
                version: 1,
                created_at: state::now_rfc3339(),
                packages: g
                    .packages
                    .iter()
                    .map(|p| LockEntry {
                        name: p.name.clone(),
                        version: p.version.clone().unwrap_or_else(|| "latest".into()),
                        backend: p.backend.clone().unwrap_or_else(|| "auto".into()),
                        kind: p.kind,
                    })
                    .collect(),
            };
            let path = group::lock_path(&ctx.prefix);
            maca::install::atomic_write(&path, serde_json::to_string_pretty(&lock)?.as_bytes())?;
            println!("exported group '{name}' to {}", path.display());
            let unpinned: Vec<&str> = lock
                .packages
                .iter()
                .filter(|p| p.version == "latest")
                .map(|p| p.name.as_str())
                .collect();
            if !unpinned.is_empty() {
                eprintln!(
                    "maca: note: {} have no version in the group and are exported as \"latest\", so `sync` accepts any version of them; `maca lock` records exact installed versions",
                    unpinned.join(", ")
                );
            }
        }
    }
    Ok(())
}

fn cmd_lock(ctx: &Ctx) -> Result<()> {
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
    let lock = group::write_lock(&ctx.prefix)?;
    println!(
        "wrote {} ({} packages)",
        group::lock_path(&ctx.prefix).display(),
        lock.packages.len()
    );
    Ok(())
}

/// The catalogs only serve their current version, so a locked version cannot
/// be requested; instead an install whose resolved version differs from the
/// lock is reported as a failure. `latest` (or no version) accepts anything.
fn check_locked_version(out: &backend::InstallOutcome, locked: Option<&str>) -> Result<()> {
    match locked {
        Some(v) if v != "latest" && v != out.version => Err(Error::Unsupported(format!(
            "{}: lock pins {v} but {} now provides {}",
            out.name, out.backend, out.version
        ))),
        _ => Ok(()),
    }
}

async fn cmd_sync(ctx: &Ctx, dry_run: bool, prune: bool) -> Result<()> {
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
    let lock = group::read_lock(&ctx.prefix)?;
    println!("syncing {} packages from lockfile", lock.packages.len());
    let snapshot = if dry_run {
        None
    } else {
        Some(snapshot::create(&ctx.prefix, "before sync", &[])?)
    };
    let result = sync_inner(ctx, &lock, dry_run, prune).await;
    if let (Err(e), Some(s)) = (&result, &snapshot) {
        match snapshot::restore(&s.id, &ctx.prefix) {
            Ok(_) => eprintln!("maca: sync failed ({e}); rolled back to snapshot {}", s.id),
            Err(r) => eprintln!("maca: sync failed ({e}); rollback also failed: {r}"),
        }
    }
    result
}

async fn sync_inner(ctx: &Ctx, lock: &group::Lockfile, dry_run: bool, prune: bool) -> Result<()> {
    let ledger = state::Ledger::load()?;
    // The ledger is shared by every prefix; sync only ever looks at, and
    // prunes, packages installed in this one.
    let cellar_root = config::cellar_dir(&ctx.prefix);
    let here: Vec<&state::Record> = ledger
        .packages
        .iter()
        .filter(|r| config::path_under(&r.cellar, &cellar_root))
        .collect();
    let installed_here = |name: &str| {
        here.iter()
            .find(|r| r.name.eq_ignore_ascii_case(name) && r.cellar.exists())
    };
    let semaphore = Arc::new(Semaphore::new(ctx.jobs));
    let mut handles = Vec::new();
    let mut ok = 0;
    let mut failed = 0;
    for entry in &lock.packages {
        // Already installed at the locked version (any version, for a
        // "latest" entry): nothing to do.
        if let Some(r) = installed_here(&entry.name)
            && (entry.version == "latest" || r.version == entry.version)
        {
            println!("{} {} (already installed)", r.name, r.version);
            ok += 1;
            continue;
        }
        let sem = semaphore.clone();
        let prefix = ctx.prefix.clone();
        let entry = entry.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let opts = InstallOptions {
                dry_run,
                prefix,
                kind: entry.kind,
                build_from_source: false,
                no_snapshot: true,
                jobs: 1,
            };
            let out = backend::install_with_backend(&entry.backend, &entry.name, &opts).await?;
            check_locked_version(&out, Some(&entry.version))?;
            Ok::<_, Error>(out)
        }));
    }
    for handle in handles {
        match handle.await {
            Ok(Ok(out)) => {
                let verb = if dry_run {
                    "[dry-run] would install"
                } else {
                    "installed"
                };
                println!("{verb} {} {} ({})", out.name, out.version, out.backend);
                ok += 1;
            }
            Ok(Err(e)) => {
                eprintln!("error: {e}");
                failed += 1;
            }
            Err(j) => {
                eprintln!("error: task panicked: {j}");
                failed += 1;
            }
        }
    }

    // Prune last, and only once everything the lock asks for is in place.
    // A package stays if the lock names it or a locked package needs it
    // (directly or through other dependencies), so pruning never breaks a
    // locked package by removing a library under it.
    if prune && failed == 0 {
        let ledger = state::Ledger::load()?; // includes what was just installed
        let mut keep: std::collections::HashSet<String> = lock
            .packages
            .iter()
            .map(|p| p.name.to_ascii_lowercase())
            .collect();
        let mut queue: Vec<String> = keep.iter().cloned().collect();
        while let Some(name) = queue.pop() {
            if let Some(r) = ledger
                .packages
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(&name))
            {
                for dep in &r.depends_on {
                    if keep.insert(dep.to_ascii_lowercase()) {
                        queue.push(dep.to_ascii_lowercase());
                    }
                }
            }
        }
        for pkg in ledger
            .packages
            .iter()
            .filter(|r| config::path_under(&r.cellar, &cellar_root))
        {
            if keep.contains(&pkg.name.to_ascii_lowercase()) {
                continue;
            }
            if dry_run {
                println!("[dry-run] would remove unlocked package {}", pkg.name);
            } else {
                println!("pruning unlocked package {}", pkg.name);
                if let Err(e) = backend::remove_with_prefix(&ctx.prefix, &pkg.name) {
                    eprintln!("error: {e}");
                    failed += 1;
                }
            }
        }
    }
    let label = if dry_run {
        "sync dry run complete"
    } else {
        "sync complete"
    };
    println!("{label}: {ok} ok, {failed} failed");
    if failed > 0 {
        return Err(Error::Other(anyhow::anyhow!(
            "sync failed for {failed} package(s)"
        )));
    }
    Ok(())
}

async fn cmd_kind(ctx: &Ctx, action: KindCommands, kind: Kind) -> Result<()> {
    match action {
        KindCommands::Install {
            packages,
            dry_run,
            build_from_source,
            no_snapshot,
            ..
        } => {
            // `ctx.backend` already resolved this subcommand's --backend.
            let opts = ctx.opts(dry_run, kind, build_from_source, no_snapshot);
            cmd_install(ctx, &packages, &opts).await
        }
        KindCommands::Remove {
            packages,
            no_snapshot,
        } => cmd_remove(ctx, &packages, no_snapshot),
        KindCommands::List => cmd_list(Some(kind), false),
    }
}

fn cmd_pin(ctx: &Ctx, package: String, version: Option<String>) -> Result<()> {
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
    let _state = state::state_lock()?; // pins.json is shared by every prefix
    let ledger = state::Ledger::load()?;
    let record = ledger
        .get(&package)
        .cloned()
        .ok_or_else(|| Error::NotFound(format!("package '{package}' is not installed")))?;
    // A pin holds the installed version. Pinning to some other version would
    // promise something `upgrade` cannot deliver.
    if let Some(v) = &version
        && v != &record.version
    {
        return Err(Error::Unsupported(format!(
            "{} is installed at {}; a pin holds the installed version, so it cannot be pinned to {v}",
            record.name, record.version
        )));
    }
    let mut pins = state::load_pins()?;
    pins.insert(record.name.clone(), record.version.clone());
    state::save_pins(&pins)?;
    println!("pinned {} to {}", record.name, record.version);
    Ok(())
}

fn cmd_unpin(ctx: &Ctx, package: String) -> Result<()> {
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
    let _state = state::state_lock()?; // pins.json is shared by every prefix
    let mut pins = state::load_pins()?;
    let key = pins
        .keys()
        .find(|k| k.eq_ignore_ascii_case(&package))
        .cloned()
        .ok_or_else(|| Error::NotFound(format!("'{package}' is not pinned")))?;
    pins.remove(&key);
    state::save_pins(&pins)?;
    println!("unpinned {key}");
    Ok(())
}

async fn cmd_why(ctx: &Ctx, package: &str) -> Result<()> {
    let ledger = state::Ledger::load()?;
    let mut dependents = Vec::new();
    let mut unknown = Vec::new();
    for record in &ledger.packages {
        if record.name == package {
            continue;
        }
        if record.backend != "brew" {
            unknown.push(record.name.as_str());
            continue;
        }
        match BrewBackend::new().dependencies(&record.name).await {
            Ok(deps) => {
                if deps.iter().any(|d| d.eq_ignore_ascii_case(package)) {
                    dependents.push(record.name.clone());
                }
            }
            Err(e) => {
                tracing::warn!("could not resolve deps of {}: {e}", record.name);
                unknown.push(record.name.as_str());
            }
        }
    }
    let _ = ctx;
    if dependents.is_empty() {
        println!("no installed brew package depends on {package}");
    } else {
        println!("installed packages that depend on {package}:");
        for name in &dependents {
            println!("  {name}");
        }
    }
    if !unknown.is_empty() {
        println!(
            "not checked (non-brew or unresolved): {}",
            unknown.join(", ")
        );
    }
    Ok(())
}

/// Pinned package names and the version each was pinned at.
pub fn pinned_versions() -> Result<BTreeMap<String, String>> {
    state::load_pins()
}
