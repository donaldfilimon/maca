//! maca CLI — cross-platform package manager (macOS/Linux, winget catalog on Windows).
//!
//! Examples:
//! ```sh
//! maca search jq
//! maca info jq
//! maca install jq --dry-run
//! maca install jq
//! maca list
//! maca remove jq
//! maca doctor
//! maca custom add --name hello --version 1.0.0 \
//!   --url https://example.com/hello.tar.gz --sha256 <hex>
//! maca custom list
//! ```

use clap::{Parser, Subcommand};

use maca::backend::{BackendKind, InstallOptions, auto_search_order};
use maca::brew::BrewBackend;
use maca::config;
use maca::custom::{Artifact, ArtifactKind, CustomBackend, Entry, Registry};
use maca::state;
use maca::winget::WingetBackend;
use maca::{Error, Result};
use std::collections::BTreeMap;

#[derive(Debug, Parser)]
#[command(name = "maca", version = maca::VERSION, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Backends to consider when `--backend auto` (search fans out to all).
    #[arg(long, global = true, value_enum, default_value_t = BackendKind::Auto)]
    backend: BackendKind,

    /// Override install prefix (also `MACA_PREFIX`).
    #[arg(long, global = true)]
    prefix: Option<std::path::PathBuf>,

    /// Less verbose output.
    #[arg(long, global = true)]
    quiet: bool,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Search packages across enabled backends.
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Show package details.
    Info { package: String },
    /// Install a package.
    Install {
        package: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// List installed packages.
    List,
    /// Upgrade installed packages to their latest catalog versions.
    Upgrade {
        /// Only upgrade these package names.
        #[arg(long)]
        package: Vec<String>,
    },
    /// Remove an installed package (deletes its cellar and ledger entry).
    Remove { package: String },
    /// Health check: prefix, backends, binaries.
    Doctor,
    /// Manage the custom registry backend.
    Custom {
        #[command(subcommand)]
        action: CustomCommands,
    },
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
        /// Artifact URL (tar.gz or single binary).
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
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "")]
        homepage: String,
    },
    /// Remove a custom package entry.
    Remove { name: String },
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
    custom_registry_urls: Vec<String>,
}

impl Ctx {
    fn from_cli(prefix: Option<std::path::PathBuf>, backend: BackendKind) -> Result<Self> {
        let cfg = config::Config::load()?;
        let resolved_backend = match cfg.default_backend.as_deref() {
            Some(s) if backend == BackendKind::Auto => BackendKind::from_str_lossy(s),
            _ => backend,
        };
        Ok(Self {
            prefix: prefix.unwrap_or_else(|| cfg.prefix()),
            backend: resolved_backend,
            custom_registry_urls: cfg.custom_registry_urls,
        })
    }
}

async fn run(cli: Cli) -> Result<()> {
    let ctx = Ctx::from_cli(cli.prefix, cli.backend)?;
    match cli.command {
        Commands::Search { query, limit } => cmd_search(&ctx, &query, limit).await,
        Commands::Info { package } => cmd_info(&ctx, &package).await,
        Commands::Install { package, dry_run } => cmd_install(&ctx, &package, dry_run).await,
        Commands::List => cmd_list(),
        Commands::Upgrade { package } => cmd_upgrade(&ctx, package).await,
        Commands::Remove { package } => cmd_remove(&ctx, &package),
        Commands::Doctor => cmd_doctor(&ctx),
        Commands::Custom { action } => cmd_custom(action).await,
    }
}

async fn cmd_search(ctx: &Ctx, query: &str, limit: usize) -> Result<()> {
    let mut all: Vec<maca::backend::Package> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    if ctx.backend == BackendKind::Auto {
        for kind in auto_search_order() {
            match search_one(ctx, kind, query, limit).await {
                Ok(mut pkgs) => all.append(&mut pkgs),
                Err(e) => errors.push(format!("{}: {e}", kind.as_str())),
            }
        }
    } else {
        all = search_one(ctx, ctx.backend, query, limit).await?;
    }

    all.sort_by(|a, b| a.name.cmp(&b.name));
    all.dedup_by(|a, b| a.name == b.name && a.backend == b.backend);
    if all.is_empty() {
        println!("no packages matching '{query}'");
    } else {
        println!(
            "{:<24} {:<12} {:<8} DESCRIPTION",
            "NAME", "VERSION", "BACKEND"
        );
        for p in &all {
            let desc: String = p.description.chars().take(48).collect();
            println!("{:<24} {:<12} {:<8} {}", p.name, p.version, p.backend, desc);
        }
    }
    for e in &errors {
        eprintln!("warning: search {e}");
    }
    Ok(())
}

async fn search_one(
    ctx: &Ctx,
    kind: BackendKind,
    query: &str,
    limit: usize,
) -> Result<Vec<maca::backend::Package>> {
    match kind {
        BackendKind::Brew => BrewBackend::new().search(query, limit).await,
        BackendKind::Winget => WingetBackend::new().search(query, limit).await,
        BackendKind::Custom => {
            let reg = Registry::load_merged(&ctx.custom_registry_urls).await?;
            CustomBackend::new(reg).search(query, limit)
        }
        BackendKind::Auto => unreachable!(),
    }
}

async fn cmd_info(ctx: &Ctx, package: &str) -> Result<()> {
    let errors = info_one(ctx, package).await;
    match errors {
        Ok(p) => {
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
        Err(e) => Err(e),
    }
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
            // Try custom -> brew -> winget in order; first hit wins.
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

async fn cmd_install(ctx: &Ctx, package: &str, dry_run: bool) -> Result<()> {
    let opts = InstallOptions {
        dry_run,
        prefix: ctx.prefix.clone(),
    };
    match ctx.backend {
        BackendKind::Brew => {
            let out = BrewBackend::new().install(package, &opts).await?;
            print_install_done(&out, dry_run);
        }
        BackendKind::Winget => {
            let out = WingetBackend::new().install(package, &opts).await?;
            print_install_done(&out, dry_run);
        }
        BackendKind::Custom => {
            let reg = Registry::load_merged(&ctx.custom_registry_urls).await?;
            let out = CustomBackend::new(reg).install(package, &opts).await?;
            print_install_done(&out, dry_run);
        }
        BackendKind::Auto => {
            // First backend whose plan() succeeds handles the install.
            let mut last_err: Option<Error> = None;
            let mut done = false;
            // Custom first (local, explicit).
            {
                let reg = Registry::load_merged(&ctx.custom_registry_urls).await?;
                let custom = CustomBackend::new(reg);
                match custom.plan(package) {
                    Ok(_) => {
                        let out = custom.install(package, &opts).await?;
                        print_install_done(&out, dry_run);
                        done = true;
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            if !done {
                match BrewBackend::new().plan(package).await {
                    Ok(_) => {
                        let out = BrewBackend::new().install(package, &opts).await?;
                        print_install_done(&out, dry_run);
                        done = true;
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            if !done {
                match WingetBackend::new().plan(package).await {
                    Ok(_) => {
                        let out = WingetBackend::new().install(package, &opts).await?;
                        print_install_done(&out, dry_run);
                        done = true;
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            if !done {
                return Err(last_err.unwrap_or_else(|| {
                    Error::NotFound(format!("package '{package}' not found in any backend"))
                }));
            }
        }
    }
    Ok(())
}

fn print_install_done(out: &maca::backend::InstallOutcome, dry_run: bool) {
    if !dry_run {
        println!(
            "installed {} {} via {} -> {}",
            out.name,
            out.version,
            out.backend,
            out.cellar_path.display()
        );
        for bin in &out.linked_bins {
            println!("  linked {}", bin.display());
        }
    }
}

fn cmd_list() -> Result<()> {
    let ledger = state::Ledger::load()?;
    if ledger.packages.is_empty() {
        println!("no packages installed");
        return Ok(());
    }
    println!("{:<24} {:<12} {:<8} CELLAR", "NAME", "VERSION", "BACKEND");
    for r in &ledger.packages {
        println!(
            "{:<24} {:<12} {:<8} {}",
            r.name,
            r.version,
            r.backend,
            r.cellar.display()
        );
    }
    Ok(())
}

async fn cmd_upgrade(ctx: &Ctx, only: Vec<String>) -> Result<()> {
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
    for record in &ledger.packages {
        if !only_set.is_empty() && !only_set.contains(&record.name.to_ascii_lowercase()) {
            continue;
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
            match install_record(ctx, &record.backend, &record.name).await {
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
        // Fall back to a lexical comparison if either is not semver.
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

async fn install_record(ctx: &Ctx, backend: &str, name: &str) -> Result<()> {
    let opts = InstallOptions {
        dry_run: false,
        prefix: ctx.prefix.clone(),
    };
    match backend {
        "brew" => {
            BrewBackend::new().install(name, &opts).await?;
        }
        "winget" => {
            WingetBackend::new().install(name, &opts).await?;
        }
        "custom" => {
            let reg = Registry::load_merged(&ctx.custom_registry_urls).await?;
            CustomBackend::new(reg).install(name, &opts).await?;
        }
        other => {
            return Err(Error::Unsupported(format!(
                "unknown backend '{other}' in ledger"
            )));
        }
    }
    Ok(())
}

fn cmd_remove(ctx: &Ctx, package: &str) -> Result<()> {
    let mut ledger = state::Ledger::load()?;
    let record = ledger
        .get(package)
        .cloned()
        .ok_or_else(|| Error::NotFound(format!("package '{package}' is not installed")))?;
    // Remove the cellar directory.
    if record.cellar.exists() {
        std::fs::remove_dir_all(&record.cellar)?;
        // Clean up empty parent `<name>/`.
        if let Some(parent) = record.cellar.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    // Remove symlinked bins that point into the removed cellar.
    // Best-effort: remove `<prefix>/bin/<exe>` entries whose target is gone.
    let bin_dir = config::bin_dir(&ctx.prefix);
    if bin_dir.exists() {
        for entry in std::fs::read_dir(&bin_dir)? {
            let entry = entry?;
            let link = entry.path();
            if let Ok(target) = read_link_target(&link) {
                if target.starts_with(&record.cellar) && !target.exists() {
                    // Only remove if the target is actually gone (post-delete).
                    let _ = std::fs::remove_file(&link);
                }
            }
        }
    }
    // Remove the Homebrew-style `<prefix>/opt/<name>` symlink.
    let opt_link = ctx.prefix.join("opt").join(package);
    if opt_link.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(&opt_link);
    }
    ledger.remove(package);
    ledger.save()?;
    // Also drop the custom registry entry if the package came from there.
    let mut reg = Registry::load_local()?;
    if reg.remove(package) {
        reg.save_local()?;
        println!("also removed custom registry entry for '{package}'");
    }
    println!("removed {package} {}", record.version);
    Ok(())
}

fn read_link_target(link: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    #[cfg(windows)]
    {
        // Windows uses plain copies; treat missing as non-link.
        let _ = link;
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "not a symlink on windows",
        ))
    }
    #[cfg(not(windows))]
    {
        std::fs::read_link(link)
    }
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
    // Backend reachability (best-effort, no hard failure).
    println!("  brew: formulae.brew.sh API");
    println!("  winget: api.winget.run + winget-pkgs manifests (install Windows-only)");
    println!(
        "  custom: {} entries",
        Registry::load_local()?.packages.len()
    );
    println!(
        "  installed: {} packages",
        state::Ledger::load()?.packages.len()
    );
    Ok(())
}

async fn cmd_custom(action: CustomCommands) -> Result<()> {
    match action {
        CustomCommands::List => {
            let reg = Registry::load_local()?;
            if reg.packages.is_empty() {
                println!("no custom packages");
                println!(
                    "registry file: {}",
                    config::custom_registry_path().display()
                );
                return Ok(());
            }
            println!("{:<24} {:<12} ARTIFACTS", "NAME", "VERSION");
            for p in &reg.packages {
                println!(
                    "{:<24} {:<12} {}",
                    p.name,
                    p.version,
                    p.artifacts
                        .keys()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
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
            description,
            homepage,
        } => {
            let artifact_kind = match kind.to_ascii_lowercase().as_str() {
                "binary" => ArtifactKind::Binary,
                "tar.gz" | "targz" | "tar-gz" => ArtifactKind::TarGz,
                other => {
                    return Err(Error::Config(format!(
                        "unknown artifact kind '{other}' (expected tar.gz or binary)"
                    )));
                }
            };
            let platforms = if platform.is_empty() {
                let host = host_platform_key();
                vec![host]
            } else {
                platform
            };
            let mut artifacts: BTreeMap<String, Artifact> = BTreeMap::new();
            for p in platforms {
                artifacts.insert(
                    p.clone(),
                    Artifact {
                        url: url.clone(),
                        sha256: sha256.clone(),
                        kind: artifact_kind,
                    },
                );
            }
            let mut reg = Registry::load_local()?;
            reg.upsert(Entry {
                name: name.clone(),
                version,
                description,
                homepage,
                artifacts,
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
