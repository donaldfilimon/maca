//! Aqua standard registry as a package source.
//!
//! Packages are read from `registry.yaml` and each package's own registry
//! file over HTTP. Installs download a `github_release` asset or a concrete
//! `http` URL. Nothing shells out to `aqua` or `gh`.
//!
//! `MACA_AQUA_INDEX` overrides the registry index URL. `MACA_AQUA_RAW`
//! overrides the raw tree that package files are fetched from. GitHub release
//! metadata uses `MACA_GITHUB_API`, the same knob as the github source.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::backend::{InstallOptions, InstallOutcome, InstallPlan, Package};
use crate::github::{self, ArtifactKind};
use crate::install::{self, sha256_from_sums};
use crate::state;
use crate::{Error, Result};

const INDEX_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const DEFAULT_INDEX: &str =
    "https://raw.githubusercontent.com/aquaproj/aqua-registry/main/registry.yaml";
const DEFAULT_RAW: &str = "https://raw.githubusercontent.com/aquaproj/aqua-registry/main";

#[derive(Default)]
pub struct AquaBackend;

impl AquaBackend {
    pub fn new() -> Self {
        Self
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Package>> {
        let text = cached_index().await?;
        Ok(search_index(&text, query, limit))
    }

    pub async fn info(&self, name: &str) -> Result<Package> {
        let slash = slash_spec(name)?;
        let pkg = load_package(&slash).await?;
        let version = latest_tag(&pkg).await.unwrap_or_else(|_| "latest".into());
        Ok(describe(&pkg, &slash, &version))
    }

    pub async fn plan(&self, name: &str) -> Result<InstallPlan> {
        Ok(self.prepare(name).await?.plan)
    }

    async fn prepare(&self, name: &str) -> Result<Prepared> {
        let slash = slash_spec(name)?;
        let cellar = slash.replace('/', "--");
        let pkg = load_package(&slash).await?;
        if pkg.kind == "http" {
            return prepare_http(&cellar, &pkg);
        }
        let owner = pkg.repo_owner.clone().unwrap_or_default();
        let repo = pkg.repo_name.clone().unwrap_or_default();
        let body = github_release_json(&owner, &repo).await?;
        let tag = body
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if tag.is_empty() {
            return Err(Error::NotFound(format!(
                "no release tag for {owner}/{repo}"
            )));
        }
        prepare_release(&cellar, &pkg, &tag, &body)
    }

    pub async fn install(&self, name: &str, opts: &InstallOptions) -> Result<InstallOutcome> {
        if opts.build_from_source {
            return Err(Error::Unsupported(
                "aqua packages have no source-build recipe".into(),
            ));
        }
        let prepared = self.prepare(name).await?;
        if opts.dry_run {
            println!(
                "[dry-run] would install {} {} via aqua ({})",
                prepared.plan.name, prepared.plan.version, prepared.plan.url
            );
            let cellar_path = crate::config::cellar_dir(&opts.prefix)
                .join(&prepared.plan.name)
                .join(&prepared.plan.version);
            return Ok(InstallOutcome {
                name: prepared.plan.name,
                version: prepared.plan.version,
                backend: "aqua".into(),
                cellar_path,
                linked_bins: vec![],
            });
        }
        let bytes = install::download(&prepared.plan.url).await?;
        let sha = resolve_sha(&prepared, &bytes).await?;
        if sha.is_none() {
            eprintln!(
                "maca: warning: aqua package {} declares no sha256; installing {} UNVERIFIED",
                prepared.plan.name, prepared.plan.url
            );
        }
        let cellar = github::commit_artifact(
            &prepared.plan.name,
            &prepared.plan.version,
            &bytes,
            sha.as_deref(),
            &opts.prefix,
            prepared.kind,
            &prepared.raw_exe,
        )?;
        let linked = link_files(&cellar, &prepared, opts)?;
        state::upsert_record(state::record_with_kind(
            &prepared.plan.name,
            &prepared.plan.version,
            "aqua",
            cellar.clone(),
            opts.kind,
        ))?;
        Ok(InstallOutcome {
            name: prepared.plan.name,
            version: prepared.plan.version,
            backend: "aqua".into(),
            cellar_path: cellar,
            linked_bins: linked,
        })
    }
}

struct Prepared {
    plan: InstallPlan,
    kind: ArtifactKind,
    raw_exe: String,
    checksum_url: Option<String>,
    checksum_name: Option<String>,
    files: Vec<(String, Option<String>)>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AquaFile {
    name: String,
    #[serde(default)]
    src: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AquaChecksum {
    #[serde(default)]
    asset: Option<String>,
    #[serde(default)]
    algorithm: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AquaOverride {
    #[serde(default)]
    goos: Option<String>,
    #[serde(default)]
    goarch: Option<String>,
    #[serde(default)]
    asset: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    replacements: BTreeMap<String, String>,
    #[serde(default)]
    files: Vec<AquaFile>,
    #[serde(default)]
    checksum: Option<AquaChecksum>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AquaPartial {
    #[serde(default)]
    version_constraint: Option<serde_yaml::Value>,
    #[serde(default)]
    asset: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    replacements: BTreeMap<String, String>,
    #[serde(default)]
    files: Vec<AquaFile>,
    #[serde(default)]
    checksum: Option<AquaChecksum>,
    #[serde(default)]
    overrides: Vec<AquaOverride>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AquaPkg {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    repo_owner: Option<String>,
    #[serde(default)]
    repo_name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    asset: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    files: Vec<AquaFile>,
    #[serde(default)]
    replacements: BTreeMap<String, String>,
    #[serde(default)]
    overrides: Vec<AquaOverride>,
    #[serde(default)]
    supported_envs: Vec<String>,
    #[serde(default)]
    version_constraint: Option<serde_yaml::Value>,
    #[serde(default)]
    version_overrides: Vec<AquaPartial>,
    #[serde(default)]
    checksum: Option<AquaChecksum>,
}

#[derive(Debug, Deserialize, Default)]
struct AquaDoc {
    #[serde(default)]
    packages: Vec<AquaPkg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub asset: String,
    pub format: String,
    pub url: Option<String>,
    pub checksum_asset: Option<String>,
    pub files: Vec<(String, Option<String>)>,
    pub repo_owner: String,
    pub repo_name: String,
}

/// `pkgs/cli/cli/registry.yaml` → `cli/cli`.
pub fn spec_from_index_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("pkgs/")?;
    let rest = rest.strip_suffix("/registry.yaml")?;
    if rest.is_empty() || rest.contains("..") {
        return None;
    }
    Some(rest.to_string())
}

pub fn search_index(yaml: &str, query: &str, limit: usize) -> Vec<Package> {
    let limit = limit.max(1);
    let q = query.trim().to_ascii_lowercase();
    let mut names = names_from_index(yaml);
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|slash| {
            if q.is_empty() {
                return true;
            }
            let cellar = slash.replace('/', "--");
            slash.to_ascii_lowercase().contains(&q) || cellar.to_ascii_lowercase().contains(&q)
        })
        .take(limit)
        .map(|slash| Package {
            name: slash.replace('/', "--"),
            version: "latest".into(),
            description: String::new(),
            homepage: format!(
                "https://github.com/aquaproj/aqua-registry/tree/main/pkgs/{slash}/registry.yaml"
            ),
            backend: "aqua".into(),
            tags: vec![format!("aqua:{slash}")],
        })
        .collect()
}

pub fn names_from_index(yaml: &str) -> Vec<String> {
    let doc: AquaDoc = serde_yaml::from_str(yaml).unwrap_or_default();
    doc.packages
        .iter()
        .filter_map(|p| p.path.as_deref().and_then(spec_from_index_path))
        .collect()
}

/// First `github_release` or concrete-capable `http` package.
/// Any other sole type fails with that type's name.
fn select_package(packages: &[AquaPkg]) -> Result<&AquaPkg> {
    if let Some(pkg) = packages
        .iter()
        .find(|p| p.kind == "github_release" || p.kind == "http")
    {
        return Ok(pkg);
    }
    let kind = packages
        .first()
        .map(|p| p.kind.as_str())
        .filter(|k| !k.is_empty())
        .unwrap_or("empty");
    Err(Error::Unsupported(format!(
        "aqua package type `{kind}` is not supported"
    )))
}

fn choose_variant(pkg: &AquaPkg, version: &str, goos: &str, goarch: &str) -> Result<Resolved> {
    let selected = select_package(std::slice::from_ref(pkg))?;
    if selected.kind == "http" {
        let url = selected.url.clone().unwrap_or_default();
        if url.contains("{{") || url.is_empty() {
            return Err(Error::Unsupported(
                "aqua http package url is missing or still has a template".into(),
            ));
        }
        return Ok(Resolved {
            asset: url.clone(),
            format: format_of(selected.format.as_deref(), &url),
            url: Some(url),
            checksum_asset: None,
            files: file_pairs(&selected.files),
            repo_owner: String::new(),
            repo_name: selected.repo_name.clone().unwrap_or_else(|| "aqua".into()),
        });
    }
    if !env_supported(&selected.supported_envs, goos, goarch) {
        return Err(Error::Unsupported(format!(
            "aqua package does not support {goos}/{goarch}"
        )));
    }
    let mut asset = selected.asset.clone();
    let mut format = selected.format.clone();
    let mut replacements = selected.replacements.clone();
    let mut files = selected.files.clone();
    let mut checksum = selected.checksum.clone();
    let mut overrides = selected.overrides.clone();
    let mut matched_override = false;
    for partial in &selected.version_overrides {
        let constraint = constraint_text(partial.version_constraint.as_ref());
        if !constraint_matches(&constraint, version) {
            continue;
        }
        matched_override = true;
        if partial.asset.is_some() {
            asset = partial.asset.clone();
        }
        if partial.format.is_some() {
            format = partial.format.clone();
        }
        if !partial.replacements.is_empty() {
            replacements = partial.replacements.clone();
        }
        if !partial.files.is_empty() {
            files = partial.files.clone();
        }
        if partial.checksum.is_some() {
            checksum = partial.checksum.clone();
        }
        if !partial.overrides.is_empty() {
            overrides = partial.overrides.clone();
        }
        break;
    }
    let root_constraint = constraint_text(selected.version_constraint.as_ref());
    if !matched_override && !constraint_matches(&root_constraint, version) {
        return Err(Error::Unsupported(format!(
            "no aqua version constraint matches {version}"
        )));
    }
    for over in &overrides {
        if !override_matches(over, goos, goarch) {
            continue;
        }
        if over.asset.is_some() {
            asset = over.asset.clone();
        }
        if over.format.is_some() {
            format = over.format.clone();
        }
        if !over.replacements.is_empty() {
            replacements.clone_from(&over.replacements);
        }
        if !over.files.is_empty() {
            files.clone_from(&over.files);
        }
        if over.checksum.is_some() {
            checksum.clone_from(&over.checksum);
        }
    }
    let format = format.unwrap_or_else(|| "tar.gz".into());
    let asset_tpl = asset.unwrap_or_default();
    let rendered = render_template(&asset_tpl, version, goos, goarch, &format, &replacements);
    if rendered.is_empty() || rendered.contains("{{") {
        return Err(Error::Unsupported(format!(
            "aqua asset template {asset_tpl:?} did not render for {version}"
        )));
    }
    let checksum_asset = checksum.as_ref().and_then(|c| {
        if c.enabled == Some(false) {
            return None;
        }
        if c.algorithm.as_deref().is_some_and(|a| a != "sha256") {
            return None;
        }
        c.asset
            .as_ref()
            .map(|tpl| render_template(tpl, version, goos, goarch, &format, &replacements))
    });
    let files = files
        .iter()
        .map(|f| {
            let src = f
                .src
                .as_ref()
                .map(|tpl| render_template(tpl, version, goos, goarch, &format, &replacements));
            (f.name.clone(), src)
        })
        .collect();
    Ok(Resolved {
        asset: rendered,
        format,
        url: None,
        checksum_asset,
        files,
        repo_owner: selected.repo_owner.clone().unwrap_or_default(),
        repo_name: selected.repo_name.clone().unwrap_or_default(),
    })
}

fn file_pairs(files: &[AquaFile]) -> Vec<(String, Option<String>)> {
    files
        .iter()
        .map(|f| (f.name.clone(), f.src.clone()))
        .collect()
}

fn override_matches(over: &AquaOverride, goos: &str, goarch: &str) -> bool {
    let os_ok = over.goos.as_deref().is_none_or(|os| os == goos);
    let arch_ok = over.goarch.as_deref().is_none_or(|arch| arch == goarch);
    os_ok && arch_ok && (over.goos.is_some() || over.goarch.is_some())
}

fn constraint_text(value: Option<&serde_yaml::Value>) -> String {
    match value {
        None => "true".into(),
        Some(serde_yaml::Value::Bool(b)) => b.to_string(),
        Some(serde_yaml::Value::String(s)) => s.clone(),
        _ => "false".into(),
    }
}

/// `true`, `false`, `semver("<= X")` (and `>=`, `>`, `<`, `=`), or `Version == "..."`.
/// Anything else does not match.
pub fn constraint_matches(constraint: &str, version: &str) -> bool {
    let c = constraint.trim();
    if c.is_empty() || c == "true" {
        return true;
    }
    if c == "false" {
        return false;
    }
    if let Some(inner) = c
        .strip_prefix("semver(\"")
        .and_then(|s| s.strip_suffix("\")"))
    {
        let inner = inner.trim();
        let (op, bound) = if let Some(rest) = inner.strip_prefix("<=") {
            ("<=", rest.trim())
        } else if let Some(rest) = inner.strip_prefix(">=") {
            (">=", rest.trim())
        } else if let Some(rest) = inner.strip_prefix('<') {
            ("<", rest.trim())
        } else if let Some(rest) = inner.strip_prefix('>') {
            (">", rest.trim())
        } else if let Some(rest) = inner.strip_prefix('=') {
            ("=", rest.trim())
        } else {
            return false;
        };
        return semver_op(op, version, bound);
    }
    if let Some(rest) = c
        .strip_prefix("Version ==")
        .or_else(|| c.strip_prefix("Version=="))
    {
        let want = rest.trim().trim_matches('"');
        return version == want || version.trim_start_matches('v') == want.trim_start_matches('v');
    }
    false
}

fn semver_op(op: &str, version: &str, bound: &str) -> bool {
    let Some(ord) = cmp_version(version, bound) else {
        return false;
    };
    match op {
        "<=" => ord.is_le(),
        ">=" => ord.is_ge(),
        "<" => ord.is_lt(),
        ">" => ord.is_gt(),
        "=" => ord.is_eq(),
        _ => false,
    }
}

fn cmp_version(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let a = a.trim().trim_start_matches('v');
    let b = b.trim().trim_start_matches('v');
    if let (Ok(x), Ok(y)) = (semver::Version::parse(a), semver::Version::parse(b)) {
        return Some(x.cmp(&y));
    }
    if a == b {
        return Some(std::cmp::Ordering::Equal);
    }
    if crate::backend::is_newer_version(a, b) {
        Some(std::cmp::Ordering::Greater)
    } else if crate::backend::is_newer_version(b, a) {
        Some(std::cmp::Ordering::Less)
    } else if !a.is_empty() && !b.is_empty() {
        // Equal under maca's version order, including `1.0` and `1.0.0`.
        Some(std::cmp::Ordering::Equal)
    } else {
        None
    }
}

/// Empty list means every platform.
///
/// A token matches the host OS (`darwin`/`linux`/`windows`), the host arch
/// (`amd64`/`arm64`), or exactly `{os}/{arch}`. An exact `{os}/{other-arch}`
/// rejects the host when the list has neither our exact pair nor the bare OS.
pub fn env_supported(envs: &[String], goos: &str, goarch: &str) -> bool {
    if envs.is_empty() {
        return true;
    }
    let pair = format!("{goos}/{goarch}");
    let other_pair = envs
        .iter()
        .any(|e| e.starts_with(&format!("{goos}/")) && e != &pair && e.matches('/').count() == 1);
    let bare_os = envs.iter().any(|e| e == goos);
    let our_pair = envs.iter().any(|e| e == &pair);
    if other_pair && !our_pair && !bare_os {
        return false;
    }
    envs.iter().any(|e| e == goos || e == goarch || e == &pair)
}

pub fn render_template(
    template: &str,
    version: &str,
    goos: &str,
    goarch: &str,
    format: &str,
    replacements: &BTreeMap<String, String>,
) -> String {
    let os = replacements
        .get(goos)
        .cloned()
        .unwrap_or_else(|| goos.into());
    let arch = replacements
        .get(goarch)
        .cloned()
        .unwrap_or_else(|| goarch.into());
    let trimmed = version.strip_prefix('v').unwrap_or(version);
    template
        .replace("{{trimV .Version}}", trimmed)
        .replace("{{.Version}}", version)
        .replace("{{.OS}}", &os)
        .replace("{{.Arch}}", &arch)
        .replace("{{.Format}}", format)
}

fn slash_spec(spec: &str) -> Result<String> {
    let spec = spec.trim().strip_prefix("aqua:").unwrap_or(spec.trim());
    let slash = if spec.contains('/') {
        spec.to_string()
    } else if spec.contains("--") {
        spec.replace("--", "/")
    } else {
        return Err(Error::Unsupported(format!(
            "aqua package {spec:?} must be name/name, name--name, or aqua:name/name"
        )));
    };
    if slash
        .split('/')
        .any(|p| p.is_empty() || p == "." || p == ".." || p.contains("--"))
        || slash.contains('\\')
    {
        return Err(Error::Unsupported(format!(
            "aqua package {spec:?} is not a valid registry name"
        )));
    }
    let cellar = slash.replace('/', "--");
    if !install::valid_component(&cellar) {
        return Err(Error::Unsupported(format!(
            "aqua package {cellar:?} is not a valid path component"
        )));
    }
    Ok(slash)
}

fn go_host() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "macos") {
        "darwin"
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
        "amd64"
    } else {
        "unknown"
    };
    (os, arch)
}

async fn cached_index() -> Result<String> {
    let cache = crate::config::data_dir().join("cache/aqua-registry.yaml");
    if let Ok(meta) = std::fs::metadata(&cache)
        && meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < INDEX_TTL)
        && let Ok(text) = std::fs::read_to_string(&cache)
    {
        return Ok(text);
    }
    match download_text(&index_url()).await {
        Ok(text) => {
            if let Some(parent) = cache.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = install::atomic_write(&cache, text.as_bytes());
            Ok(text)
        }
        Err(e) => std::fs::read_to_string(&cache).map_err(|_| e),
    }
}

fn index_url() -> String {
    std::env::var("MACA_AQUA_INDEX")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_INDEX.into())
}

fn raw_base() -> String {
    std::env::var("MACA_AQUA_RAW")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RAW.into())
        .trim_end_matches('/')
        .to_string()
}

async fn load_package(slash: &str) -> Result<AquaPkg> {
    let url = format!("{}/pkgs/{slash}/registry.yaml", raw_base());
    let text = download_text(&url).await?;
    let doc: AquaDoc = serde_yaml::from_str(&text)
        .map_err(|e| Error::Config(format!("parse aqua package {slash}: {e}")))?;
    select_package(&doc.packages).cloned()
}

async fn latest_tag(pkg: &AquaPkg) -> Result<String> {
    let owner = pkg.repo_owner.as_deref().unwrap_or("");
    let repo = pkg.repo_name.as_deref().unwrap_or("");
    if owner.is_empty() || repo.is_empty() {
        return Err(Error::Unsupported(
            "aqua package has no repo_owner/repo_name".into(),
        ));
    }
    let body = github_release_json(owner, repo).await?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if tag.is_empty() {
        return Err(Error::NotFound(format!(
            "no release tag for {owner}/{repo}"
        )));
    }
    Ok(tag)
}

async fn github_release_json(owner: &str, repo: &str) -> Result<serde_json::Value> {
    let base = std::env::var("MACA_GITHUB_API")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://api.github.com".into());
    let url = format!(
        "{}/repos/{owner}/{repo}/releases/latest",
        base.trim_end_matches('/')
    );
    let client = install::http_client()?;
    let mut req = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", format!("maca/{}", crate::VERSION));
    if let Ok(token) = std::env::var("MACA_GITHUB_TOKEN").or_else(|_| std::env::var("GITHUB_TOKEN"))
        && !token.trim().is_empty()
    {
        req = req.bearer_auth(token);
    }
    let response = req
        .send()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(Error::Network(format!(
            "github release for {owner}/{repo} returned {status}"
        )));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn prepare_http(cellar: &str, pkg: &AquaPkg) -> Result<Prepared> {
    let resolved = choose_variant(pkg, "latest", "linux", "amd64")?;
    let url = resolved.url.clone().unwrap_or_default();
    let version = github::sanitize_version("latest")?;
    let kind = kind_from_format(&resolved.format, &url);
    Ok(Prepared {
        plan: InstallPlan {
            name: cellar.to_string(),
            version,
            backend: "aqua".into(),
            url,
            sha256: None,
            note: Some("aqua http url".into()),
        },
        kind,
        raw_exe: resolved
            .files
            .first()
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| resolved.repo_name.clone()),
        checksum_url: None,
        checksum_name: None,
        files: resolved.files,
    })
}

fn prepare_release(
    cellar: &str,
    pkg: &AquaPkg,
    tag: &str,
    body: &serde_json::Value,
) -> Result<Prepared> {
    let (goos, goarch) = go_host();
    let resolved = choose_variant(pkg, tag, goos, goarch)?;
    let (tag_name, assets) = github::assets_from_release(body);
    let version = github::sanitize_version(if tag_name.is_empty() { tag } else { &tag_name })?;
    if !assets.is_empty() && !assets.iter().any(|a| a.name == resolved.asset) {
        let shown: Vec<&str> = assets.iter().take(8).map(|a| a.name.as_str()).collect();
        return Err(Error::Unsupported(format!(
            "aqua rendered asset `{}` is not in the release (have: {})",
            resolved.asset,
            shown.join(", ")
        )));
    }
    let tag_for_url = if tag_name.is_empty() { tag } else { &tag_name };
    let url = format!(
        "https://github.com/{}/{}/releases/download/{tag_for_url}/{}",
        resolved.repo_owner, resolved.repo_name, resolved.asset
    );
    let checksum_url = resolved.checksum_asset.as_ref().and_then(|name| {
        if name.contains("{{") {
            return None;
        }
        Some(format!(
            "https://github.com/{}/{}/releases/download/{tag_for_url}/{name}",
            resolved.repo_owner, resolved.repo_name
        ))
    });
    let kind = kind_from_format(&resolved.format, &resolved.asset);
    let raw_exe = resolved
        .files
        .first()
        .map(|(n, _)| n.clone())
        .filter(|n| install::valid_component(n))
        .unwrap_or_else(|| resolved.repo_name.clone());
    Ok(Prepared {
        plan: InstallPlan {
            name: cellar.to_string(),
            version,
            backend: "aqua".into(),
            url,
            sha256: None,
            note: Some(format!("asset `{}`", resolved.asset)),
        },
        kind,
        raw_exe,
        checksum_url,
        checksum_name: resolved.checksum_asset,
        files: resolved.files,
    })
}

fn kind_from_format(format: &str, name: &str) -> ArtifactKind {
    match format {
        "zip" => ArtifactKind::Zip,
        "raw" => ArtifactKind::Raw,
        "tar.gz" => ArtifactKind::TarGz,
        _ => github::artifact_kind(name).unwrap_or(ArtifactKind::TarGz),
    }
}

fn format_of(format: Option<&str>, url: &str) -> String {
    if let Some(format) = format {
        return format.to_string();
    }
    let n = url.to_ascii_lowercase();
    if n.ends_with(".zip") {
        "zip".into()
    } else if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
        "tar.gz".into()
    } else {
        "raw".into()
    }
}

async fn resolve_sha(prepared: &Prepared, _bytes: &[u8]) -> Result<Option<String>> {
    let Some(url) = &prepared.checksum_url else {
        return Ok(None);
    };
    let Some(name) = &prepared.checksum_name else {
        return Ok(None);
    };
    let text = download_text(url).await?;
    Ok(sha256_from_sums(
        &text,
        prepared
            .plan
            .note
            .as_deref()
            .and_then(asset_note)
            .unwrap_or(name),
    ))
}

fn asset_note(note: &str) -> Option<&str> {
    note.strip_prefix("asset `")?.strip_suffix('`')
}

fn link_files(
    cellar: &Path,
    prepared: &Prepared,
    opts: &InstallOptions,
) -> Result<Vec<std::path::PathBuf>> {
    if prepared.kind == ArtifactKind::Raw || prepared.files.is_empty() {
        if prepared.kind == ArtifactKind::Raw {
            let exe = cellar.join("bin").join(&prepared.raw_exe);
            return Ok(vec![install::link_single_binary(
                &exe,
                &prepared.raw_exe,
                &opts.prefix,
            )?]);
        }
        let bins = install::link_bins(cellar, &[], &opts.prefix)?;
        install::link_share(cellar, &opts.prefix)?;
        return Ok(bins);
    }
    let mut linked = Vec::new();
    for (name, src) in &prepared.files {
        if !install::valid_component(name) {
            return Err(Error::Unsupported(format!(
                "aqua file name {name:?} is not a valid path component"
            )));
        }
        let rel = src.clone().unwrap_or_else(|| format!("bin/{name}"));
        if !safe_rel(&rel) {
            return Err(Error::Unsupported(format!(
                "aqua file src {rel:?} escapes the keg"
            )));
        }
        let full = cellar.join(&rel);
        if !full.starts_with(cellar) {
            return Err(Error::Unsupported(format!(
                "aqua file src {rel:?} escapes the keg"
            )));
        }
        linked.push(install::link_single_binary(&full, name, &opts.prefix)?);
    }
    install::link_share(cellar, &opts.prefix)?;
    Ok(linked)
}

fn safe_rel(rel: &str) -> bool {
    let path = Path::new(rel);
    !path.is_absolute()
        && path.components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

fn describe(pkg: &AquaPkg, slash: &str, version: &str) -> Package {
    let version = github::sanitize_version(version).unwrap_or_else(|_| "unknown".into());
    Package {
        name: slash.replace('/', "--"),
        version,
        description: pkg.description.clone(),
        homepage: match (&pkg.repo_owner, &pkg.repo_name) {
            (Some(o), Some(r)) => format!("https://github.com/{o}/{r}"),
            _ => String::new(),
        },
        backend: "aqua".into(),
        tags: vec![format!("aqua:{slash}")],
    }
}

async fn download_text(url: &str) -> Result<String> {
    let bytes = install::download(url).await?;
    String::from_utf8(bytes)
        .map_err(|e| Error::Other(anyhow::anyhow!("response is not utf-8: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_path_becomes_a_cellar_safe_name() {
        assert_eq!(
            spec_from_index_path("pkgs/cli/cli/registry.yaml").as_deref(),
            Some("cli/cli")
        );
        let pkgs = search_index(
            "packages:\n- type: github_content\n  path: pkgs/cli/cli/registry.yaml\n- path: pkgs/bad/../x/registry.yaml\n",
            "cli",
            10,
        );
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "cli--cli");
        assert_eq!(pkgs[0].backend, "aqua");
        assert!(install::valid_component(&pkgs[0].name));
    }

    #[test]
    fn template_applies_replacements_and_trim_v() {
        let mut repl = BTreeMap::new();
        repl.insert("darwin".into(), "macOS".into());
        repl.insert("amd64".into(), "x86_64".into());
        let rendered = render_template(
            "gh_{{trimV .Version}}_{{.OS}}_{{.Arch}}.{{.Format}}",
            "v2.40.0",
            "darwin",
            "amd64",
            "tar.gz",
            &repl,
        );
        assert_eq!(rendered, "gh_2.40.0_macOS_x86_64.tar.gz");
    }

    #[test]
    fn constraints_and_unsupported_types() {
        assert!(constraint_matches("true", "1.2.3"));
        assert!(!constraint_matches("false", "1.2.3"));
        assert!(constraint_matches("semver(\"<= 0.5.2\")", "0.5.2"));
        assert!(!constraint_matches("semver(\"<= 0.5.2\")", "0.5.3"));
        assert!(constraint_matches("semver(\">= 1.0.0\")", "1.2.0"));
        assert!(constraint_matches("Version == \"v1.0.0\"", "v1.0.0"));
        assert!(!constraint_matches("nope(\"1\")", "1"));
        let cargo = AquaPkg {
            kind: "cargo".into(),
            ..AquaPkg::default()
        };
        let err = select_package(&[cargo]).unwrap_err();
        assert!(err.to_string().contains("`cargo`"), "{err}");
    }

    #[test]
    fn supported_envs_reject_the_other_arch_pair() {
        assert!(env_supported(&[], "darwin", "arm64"));
        assert!(env_supported(
            &["darwin".into(), "amd64".into()],
            "darwin",
            "arm64"
        ));
        assert!(!env_supported(&["linux/amd64".into()], "linux", "arm64"));
        assert!(env_supported(
            &["linux/amd64".into(), "linux/arm64".into()],
            "linux",
            "arm64"
        ));
    }

    #[test]
    fn version_override_and_platform_override_render() {
        let yaml = r#"
packages:
  - type: github_release
    repo_owner: cli
    repo_name: cli
    description: GitHub CLI
    version_constraint: "false"
    asset: unused
    files:
      - name: gh
        src: gh_{{trimV .Version}}_{{.OS}}_{{.Arch}}/bin/gh
    version_overrides:
      - version_constraint: semver("<= 0.5.2")
        asset: old_{{.Version}}.tar.gz
      - version_constraint: semver(">= 0.5.3")
        asset: gh_{{trimV .Version}}_{{.OS}}_{{.Arch}}.{{.Format}}
        format: tar.gz
        replacements:
          darwin: macOS
          amd64: x86_64
        overrides:
          - goos: windows
            format: zip
            asset: gh_{{trimV .Version}}_windows_{{.Arch}}.zip
        checksum:
          type: github_release
          asset: gh_{{trimV .Version}}_checksums.txt
          algorithm: sha256
"#;
        let doc: AquaDoc = serde_yaml::from_str(yaml).unwrap();
        let pkg = select_package(&doc.packages).unwrap();
        let resolved = choose_variant(pkg, "v2.40.0", "darwin", "amd64").unwrap();
        assert_eq!(resolved.asset, "gh_2.40.0_macOS_x86_64.tar.gz");
        assert_eq!(
            resolved.checksum_asset.as_deref(),
            Some("gh_2.40.0_checksums.txt")
        );
        let windows = choose_variant(pkg, "v2.40.0", "windows", "amd64").unwrap();
        assert_eq!(windows.format, "zip");
        assert!(windows.asset.contains("windows"));
    }

    #[test]
    fn http_url_with_a_template_is_refused() {
        let pkg = AquaPkg {
            kind: "http".into(),
            url: Some("https://example.test/{{.Version}}/tool.tar.gz".into()),
            ..AquaPkg::default()
        };
        let err = choose_variant(&pkg, "v1", "linux", "amd64").unwrap_err();
        assert!(err.to_string().contains("template"), "{err}");
        let ok = AquaPkg {
            kind: "http".into(),
            url: Some("https://example.test/tool.tar.gz".into()),
            repo_name: Some("tool".into()),
            ..AquaPkg::default()
        };
        let resolved = choose_variant(&ok, "v1", "linux", "amd64").unwrap();
        assert_eq!(
            resolved.url.as_deref(),
            Some("https://example.test/tool.tar.gz")
        );
    }

    #[test]
    fn root_false_constraint_without_a_match_fails() {
        let pkg = AquaPkg {
            kind: "github_release".into(),
            repo_owner: Some("a".into()),
            repo_name: Some("b".into()),
            version_constraint: Some(serde_yaml::Value::String("false".into())),
            version_overrides: vec![AquaPartial {
                version_constraint: Some(serde_yaml::Value::String("semver(\"<= 0.1.0\")".into())),
                asset: Some("old.tgz".into()),
                ..AquaPartial::default()
            }],
            ..AquaPkg::default()
        };
        let err = choose_variant(&pkg, "v2.0.0", "linux", "amd64").unwrap_err();
        assert!(err.to_string().contains("constraint"), "{err}");
    }
}
