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

use clap::{Parser, Subcommand};
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
        #[arg(long, value_enum, default_value_t = BackendKind::Auto)]
        backend: BackendKind,
        /// Output JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show package details.
    Info {
        package: String,
        #[arg(long, value_enum, default_value_t = BackendKind::Auto)]
        backend: BackendKind,
        #[arg(long)]
        json: bool,
    },
    /// Install one or more packages (concurrently).
    Install {
        #[arg(required = true)]
        packages: Vec<String>,
        #[arg(long, value_enum, default_value_t = BackendKind::Auto)]
        backend: BackendKind,
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
    /// Health check: prefix, backends, counts.
    Doctor,
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
        #[arg(long, value_enum, default_value_t = BackendKind::Auto)]
        backend: BackendKind,
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
        backend: BackendKind,
        jobs: usize,
    ) -> Result<Self> {
        let cfg = config::Config::load()?;
        let resolved_backend = match cfg.default_backend.as_deref() {
            Some(s) if backend == BackendKind::Auto => BackendKind::from_str_lossy(s),
            _ => backend,
        };
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
        _ => BackendKind::Auto,
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
        Commands::Doctor => cmd_doctor(&ctx),
        Commands::Tui => {
            let _lock = PrefixLock::acquire(&ctx.prefix)?;
            maca::tui::run(ctx.prefix.clone()).await
        }
        Commands::SelfUpdate { check, force } => cmd_self_update(check, force).await,
        Commands::Snapshot { action } => cmd_snapshot(&ctx, action),
        Commands::Custom { action } => cmd_custom(action),
        Commands::Group { action } => cmd_group(&ctx, action).await,
        Commands::Lock => cmd_lock(&ctx),
        Commands::Sync { dry_run } => cmd_sync(&ctx, dry_run).await,
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
    if let Err(e) = &result {
        if let Some(s) = &snapshot {
            match snapshot::restore(&s.id, &ctx.prefix) {
                Ok(_) => eprintln!(
                    "maca: install failed ({e}); rolled back to snapshot {}",
                    s.id
                ),
                Err(r) => eprintln!("maca: install failed ({e}); rollback also failed: {r}"),
            }
        }
    }
    match result {
        Ok(upgraded) => {
            if upgraded > 0 {
                println!("installed {upgraded} package(s)");
            }
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

async fn install_many(ctx: &Ctx, packages: &[String], opts: &InstallOptions) -> Result<usize> {
    let semaphore = Arc::new(Semaphore::new(ctx.jobs));
    let mut handles = Vec::new();
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
            match backend {
                BackendKind::Brew => BrewBackend::new().install(&name, &opts).await,
                BackendKind::Winget => WingetBackend::new().install(&name, &opts).await,
                BackendKind::Custom => {
                    let reg = Registry::load_local()?;
                    CustomBackend::new(reg).install(&name, &opts).await
                }
                BackendKind::Auto => backend::install_with_backend("auto", &name, &opts).await,
            }
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
    println!(
        "{:<24} {:<12} {:<8} {:<8} CELLAR",
        "NAME", "VERSION", "BACKEND", "KIND"
    );
    for r in rows {
        println!(
            "{:<24} {:<12} {:<8} {:<8} {}",
            r.name,
            r.version,
            r.backend,
            r.kind.as_str(),
            r.cellar.display()
        );
    }
    Ok(())
}

async fn cmd_upgrade(ctx: &Ctx, only: Vec<String>, no_snapshot: bool) -> Result<()> {
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
    let snapshot = if no_snapshot {
        None
    } else {
        Some(snapshot::create(&ctx.prefix, "before upgrade", &[])?)
    };
    let result = upgrade_inner(ctx, &only).await;
    if let Err(e) = &result {
        if let Some(s) = &snapshot {
            match snapshot::restore(&s.id, &ctx.prefix) {
                Ok(_) => eprintln!(
                    "maca: upgrade failed ({e}); rolled back to snapshot {}",
                    s.id
                ),
                Err(r) => eprintln!("maca: upgrade failed ({e}); rollback also failed: {r}"),
            }
        }
    }
    result
}

async fn upgrade_inner(ctx: &Ctx, only: &[String]) -> Result<()> {
    let ledger = state::Ledger::load()?;
    if ledger.packages.is_empty() {
        println!("no packages installed");
        return Ok(());
    }
    let only_set: std::collections::HashSet<String> =
        only.iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut upgraded = 0usize;
    let mut current = 0usize;
    let mut failed = 0usize;
    let pins = pinned_versions().unwrap_or_default();
    for record in &ledger.packages {
        if !only_set.is_empty() && !only_set.contains(&record.name.to_ascii_lowercase()) {
            continue;
        }
        if let Some(pin) = pins.get(&record.name) {
            if pin == &record.version {
                println!("{} {} is pinned; skipping", record.name, record.version);
                current += 1;
                continue;
            }
        }
        let latest = match latest_version(ctx, &record.backend, &record.name).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: could not check {}: {e}", record.name);
                failed += 1;
                continue;
            }
        };
        let Some(latest) = latest else {
            println!("{} {} is up to date", record.name, record.version);
            current += 1;
            continue;
        };
        if is_newer(&latest, &record.version) {
            println!(
                "upgrading {} {} -> {} ({})",
                record.name, record.version, latest, record.backend
            );
            let opts = InstallOptions {
                dry_run: false,
                prefix: ctx.prefix.clone(),
                kind: record.kind,
                build_from_source: false,
                no_snapshot: true,
                jobs: ctx.jobs,
            };
            match backend::install_with_backend(&record.backend, &record.name, &opts).await {
                Ok(_) => upgraded += 1,
                Err(e) => {
                    eprintln!("error: upgrade of {} failed: {e}", record.name);
                    failed += 1;
                }
            }
        } else {
            println!("{} {} is up to date", record.name, record.version);
            current += 1;
        }
    }
    println!("upgrade complete: {upgraded} upgraded, {current} current, {failed} failed");
    Ok(())
}

fn is_newer(candidate: &str, installed: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(installed),
    ) {
        (Ok(c), Ok(i)) => c > i,
        _ => candidate != installed,
    }
}

async fn latest_version(ctx: &Ctx, backend: &str, name: &str) -> Result<Option<String>> {
    let pkg = match backend {
        "brew" => BrewBackend::new().info(name).await?,
        "winget" => WingetBackend::new().info(name).await?,
        "custom" => {
            let reg = Registry::load_merged(&ctx.custom_registry_urls).await?;
            CustomBackend::new(reg).info(name)?
        }
        other => {
            return Err(Error::Unsupported(format!(
                "unknown backend '{other}' in ledger"
            )));
        }
    };
    Ok(Some(pkg.version))
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

fn cmd_doctor(ctx: &Ctx) -> Result<()> {
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
    println!("  brew: formulae.brew.sh API + GHCR bottle blobs");
    println!("  winget: api.winget.run + winget-pkgs manifests (install Windows-only)");
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
    Ok(())
}

fn cmd_custom(action: CustomCommands) -> Result<()> {
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
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
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
    }
    Ok(())
}

async fn cmd_group(ctx: &Ctx, action: GroupCommands) -> Result<()> {
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
            let g = Group::load(&name)?;
            let _lock = PrefixLock::acquire(&ctx.prefix)?;
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
                    backend::install_with_backend(
                        entry.backend.as_deref().unwrap_or("auto"),
                        &entry.name,
                        &opts,
                    )
                    .await
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
                    })
                    .collect(),
            };
            let path = group::lock_path(&ctx.prefix);
            std::fs::write(&path, serde_json::to_string_pretty(&lock)?)?;
            println!("exported group '{name}' to {}", path.display());
        }
    }
    Ok(())
}

fn cmd_lock(ctx: &Ctx) -> Result<()> {
    let lock = group::write_lock(&ctx.prefix)?;
    println!(
        "wrote {} ({} packages)",
        group::lock_path(&ctx.prefix).display(),
        lock.packages.len()
    );
    Ok(())
}

async fn cmd_sync(ctx: &Ctx, dry_run: bool) -> Result<()> {
    let _lock = PrefixLock::acquire(&ctx.prefix)?;
    let lock = group::read_lock(&ctx.prefix)?;
    println!("syncing {} packages from lockfile", lock.packages.len());
    let semaphore = Arc::new(Semaphore::new(ctx.jobs));
    let mut handles = Vec::new();
    for entry in &lock.packages {
        let sem = semaphore.clone();
        let prefix = ctx.prefix.clone();
        let entry = entry.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let opts = InstallOptions {
                dry_run,
                prefix,
                kind: Kind::App,
                build_from_source: false,
                no_snapshot: true,
                jobs: 1,
            };
            backend::install_with_backend(&entry.backend, &entry.name, &opts).await
        }));
    }
    let mut ok = 0;
    let mut failed = 0;
    for handle in handles {
        match handle.await {
            Ok(Ok(out)) => {
                println!("{} {} ({})", out.name, out.version, out.backend);
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
    println!("sync complete: {ok} ok, {failed} failed");
    Ok(())
}

async fn cmd_kind(ctx: &Ctx, action: KindCommands, kind: Kind) -> Result<()> {
    match action {
        KindCommands::Install {
            packages,
            backend,
            dry_run,
            build_from_source,
            no_snapshot,
        } => {
            let ctx = Ctx {
                prefix: ctx.prefix.clone(),
                backend,
                jobs: ctx.jobs,
                custom_registry_urls: ctx.custom_registry_urls.clone(),
            };
            let opts = ctx.opts(dry_run, kind, build_from_source, no_snapshot);
            cmd_install(&ctx, &packages, &opts).await
        }
        KindCommands::Remove {
            packages,
            no_snapshot,
        } => cmd_remove(ctx, &packages, no_snapshot),
        KindCommands::List => cmd_list(Some(kind), false),
    }
}

fn cmd_pin(_ctx: &Ctx, package: String, version: Option<String>) -> Result<()> {
    let ledger = state::Ledger::load()?;
    let record = ledger
        .get(&package)
        .cloned()
        .ok_or_else(|| Error::NotFound(format!("package '{package}' is not installed")))?;
    let version = version.unwrap_or_else(|| record.version.clone());
    let path = config::data_dir().join("pins.json");
    let mut pins: serde_json::Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path)?)?
    } else {
        serde_json::json!({})
    };
    pins[&package] = serde_json::Value::String(version.clone());
    std::fs::write(&path, serde_json::to_string_pretty(&pins)?)?;
    println!("pinned {package} to {version}");
    Ok(())
}

fn cmd_unpin(_ctx: &Ctx, package: String) -> Result<()> {
    let path = config::data_dir().join("pins.json");
    if !path.exists() {
        return Err(Error::NotFound(format!("'{package}' is not pinned")));
    }
    let mut pins: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    if pins.get(&package).is_some() {
        pins[&package] = serde_json::Value::Null;
        // Drop null entries.
        if let Some(obj) = pins.as_object_mut() {
            obj.remove(&package);
        }
        std::fs::write(&path, serde_json::to_string_pretty(&pins)?)?;
        println!("unpinned {package}");
        Ok(())
    } else {
        Err(Error::NotFound(format!("'{package}' is not pinned")))
    }
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

/// Pins are stored as a simple JSON map; used by upgrade to skip pinned names.
pub fn pinned_versions() -> Result<BTreeMap<String, String>> {
    let path = config::data_dir().join("pins.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    let mut out = BTreeMap::new();
    if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    Ok(out)
}
