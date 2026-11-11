use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn maca_bin() -> PathBuf {
    // Cargo builds the binary for integration tests and exports its path.
    PathBuf::from(env!("CARGO_BIN_EXE_maca"))
}

/// A throwaway data dir for one test, removed when dropped. Tests run in
/// parallel, so each gets its own.
struct TempHome(PathBuf);

impl TempHome {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "maca-it-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// A `maca` command confined to this home: it never reads or writes the
    /// developer's real `~/.maca`, and shell detection is not inherited.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(maca_bin());
        cmd.env("MACA_HOME", &self.0)
            .env_remove("MACA_PREFIX")
            .env_remove("SHELL");
        cmd
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn test_version_and_help() {
    let home = TempHome::new("version");
    let output = home.cmd().arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("maca {}", env!("CARGO_PKG_VERSION"))));

    let help = home.cmd().arg("--help").output().unwrap();
    assert!(help.status.success());
    let help_out = String::from_utf8_lossy(&help.stdout);
    for cmd in ["shellenv", "completions", "outdated", "clean"] {
        assert!(help_out.contains(cmd), "--help lacks {cmd}");
    }
}

#[test]
fn test_shellenv_formats() {
    let home = TempHome::new("shellenv");
    let out = home
        .cmd()
        .args(["shellenv", "--shell", "bash"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("export MACA_PREFIX="));
    assert!(text.contains("export PATH="));
    assert!(text.contains("export MANPATH="));

    let fish_out = home
        .cmd()
        .args(["shellenv", "--shell", "fish"])
        .output()
        .unwrap();
    assert!(fish_out.status.success());
    let fish_text = String::from_utf8_lossy(&fish_out.stdout);
    assert!(fish_text.contains("set -gx MACA_PREFIX"));
    assert!(fish_text.contains("fish_add_path"));

    // Auto-detection follows $SHELL.
    let auto = home
        .cmd()
        .env("SHELL", "/usr/bin/fish")
        .arg("shellenv")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&auto.stdout).contains("set -gx MACA_PREFIX"));
}

#[test]
fn test_completions() {
    let home = TempHome::new("completions");
    for shell in ["bash", "zsh", "fish"] {
        let out = home.cmd().args(["completions", shell]).output().unwrap();
        assert!(out.status.success(), "completions for {shell} failed");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("search") || text.contains("install"));
    }
}

#[test]
fn test_outdated_with_nothing_installed() {
    let home = TempHome::new("outdated");
    let out = home.cmd().args(["outdated", "--json"]).output().unwrap();
    assert!(out.status.success());
    let val: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(val, serde_json::json!([]));
}

#[test]
fn test_clean_command() {
    let home = TempHome::new("clean");
    let out = home
        .cmd()
        .args(["clean", "--keep-snapshots", "10"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("clean complete"));
}

#[test]
fn test_doctor_command() {
    let home = TempHome::new("doctor");
    let out = home.cmd().arg("doctor").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("maca doctor"));
    assert!(text.contains("doctor summary:"));
}

#[test]
fn test_binary_discovery_and_share_linking() {
    let temp = TempHome::new("bins");
    let prefix = temp.0.join("prefix");
    let cellar = prefix.join("cellar/demo/1.2.3");
    let bin_dir = cellar.join("bin");
    let sbin_dir = cellar.join("sbin");
    let share_dir = cellar.join("share/man/man1");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&sbin_dir).unwrap();
    std::fs::create_dir_all(&share_dir).unwrap();
    std::fs::write(bin_dir.join("demo_cli"), b"#!/bin/sh\necho ok").unwrap();
    std::fs::write(sbin_dir.join("demo_admin"), b"#!/bin/sh\necho admin").unwrap();
    std::fs::write(share_dir.join("demo.1"), b".TH DEMO 1").unwrap();

    let linked_bins = maca::install::link_bins(&cellar, &[], &prefix).unwrap();
    assert_eq!(linked_bins.len(), 2);
    assert!(prefix.join("bin/demo_cli").exists());
    assert!(prefix.join("bin/demo_admin").exists());

    let linked_share = maca::install::link_share(&cellar, &prefix).unwrap();
    assert_eq!(linked_share.len(), 1);
    assert!(prefix.join("share/man/man1/demo.1").exists());

    let unlinked = maca::install::unlink_share(&cellar, &prefix).unwrap();
    assert_eq!(unlinked, 1);
    assert!(!prefix.join("share/man/man1/demo.1").exists());
}

#[test]
fn a_broken_project_file_stops_the_command_instead_of_using_the_global_prefix() {
    let home = TempHome::new("badproject");
    let proj = home.0.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("maca.toml"), "prefix = [oops\n").unwrap();
    let out = home.cmd().arg("list").current_dir(&proj).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("maca.toml"), "{err}");

    // A good one scopes the prefix: shellenv reports it.
    std::fs::write(proj.join("maca.toml"), "prefix = \"scoped\"\n").unwrap();
    let out = home
        .cmd()
        .args(["shellenv", "--shell", "bash"])
        .current_dir(&proj)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("scoped"), "{text}");
}

#[test]
fn test_list_command() {
    let home = TempHome::new("list");
    let out = home.cmd().arg("list").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("PINNED") || text.contains("no packages installed"));
}

#[test]
fn test_sync_dry_run() {
    let home = TempHome::new("sync");
    // Lock the (empty) state, then dry-run a sync with prune.
    assert!(home.cmd().arg("lock").output().unwrap().status.success());
    let sync_out = home
        .cmd()
        .args(["sync", "--dry-run", "--prune"])
        .output()
        .unwrap();
    assert!(sync_out.status.success());
    let text = String::from_utf8_lossy(&sync_out.stdout);
    assert!(
        text.contains("sync dry run complete: 0 ok, 0 failed"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// Hermetic end-to-end tests: a loopback HTTP server stands in for an artifact
// host, and each test gets its own MACA_HOME.
// ---------------------------------------------------------------------------

// The e2e helpers serve shell-script packages, so they exist only on Unix.
#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::sync::Arc;

/// Serve `routes` (path -> body) on 127.0.0.1 for the life of the test process.
#[cfg(unix)]
fn serve(routes: HashMap<String, Vec<u8>>) -> String {
    serve_on(std::net::TcpListener::bind("127.0.0.1:0").unwrap(), routes)
}

/// Like `serve`, on a listener the caller already bound, for a test whose
/// bodies must contain the server's own URL.
#[cfg(unix)]
fn serve_on(listener: std::net::TcpListener, routes: HashMap<String, Vec<u8>>) -> String {
    let addr = listener.local_addr().unwrap();
    let routes = Arc::new(routes);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let routes = routes.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, body) = match routes.get(&path) {
                    Some(body) => ("200 OK", body.clone()),
                    None => ("404 Not Found", Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            });
        }
    });
    format!("http://{addr}")
}

/// A tar.gz holding one executable shell script at `<top>/bin/<exe>`.
#[cfg(unix)]
fn script_tarball(top: &str, exe: &str, prints: &str) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let body = format!("#!/bin/sh\necho {prints}\n");
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    tar.append_data(&mut header, format!("{top}/bin/{exe}"), body.as_bytes())
        .unwrap();
    tar.into_inner().unwrap().finish().unwrap()
}

/// A tar.gz of `(path, mode, content)` entries.
#[cfg(unix)]
fn tarball(entries: &[(&str, u32, &str)]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (path, mode, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(*mode);
        header.set_cksum();
        tar.append_data(&mut header, path, body.as_bytes()).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

#[cfg(unix)]
fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

#[cfg(unix)]
struct Sandbox {
    bin: PathBuf,
    home: PathBuf,
}

#[cfg(unix)]
impl Sandbox {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!("maca-e2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Self {
            bin: maca_bin(),
            home,
        }
    }

    fn prefix(&self) -> PathBuf {
        self.home.join("prefix")
    }

    /// Run maca in the sandbox; returns (success, stdout + stderr).
    fn run(&self, args: &[&str]) -> (bool, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, &str)]) -> (bool, String) {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .env("MACA_HOME", &self.home)
            .env_remove("MACA_PREFIX");
        for (key, value) in extra {
            cmd.env(key, value);
        }
        let out = cmd.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    fn ok(&self, args: &[&str]) -> String {
        let (success, text) = self.run(args);
        assert!(success, "maca {args:?} failed:\n{text}");
        text
    }

    fn fails(&self, args: &[&str]) -> String {
        let (success, text) = self.run(args);
        assert!(!success, "maca {args:?} unexpectedly succeeded:\n{text}");
        text
    }

    fn add(&self, name: &str, version: &str, url: &str, sha: &str) {
        self.ok(&[
            "custom",
            "add",
            "--name",
            name,
            "--version",
            version,
            "--url",
            url,
            "--sha256",
            sha,
        ]);
    }

    fn installed_version(&self, name: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.home.join("installed.json")).ok()?;
        let ledger: serde_json::Value = serde_json::from_str(&text).ok()?;
        ledger["packages"]
            .as_array()?
            .iter()
            .find(|p| p["name"] == name)
            .and_then(|p| p["version"].as_str().map(String::from))
    }

    fn exec(&self, exe: &str) -> String {
        let out = Command::new(self.prefix().join("bin").join(exe))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn snapshot_with_reason(&self, reason: &str) -> String {
        let mut ids: Vec<String> = std::fs::read_dir(self.home.join("snapshots"))
            .unwrap()
            .flatten()
            .filter_map(|e| {
                let text = std::fs::read_to_string(e.path().join("manifest.json")).ok()?;
                let m: serde_json::Value = serde_json::from_str(&text).ok()?;
                (m["reason"] == reason).then(|| m["id"].as_str().unwrap().to_string())
            })
            .collect();
        ids.sort();
        ids.pop()
            .unwrap_or_else(|| panic!("no snapshot with reason {reason:?}"))
    }
}

#[cfg(unix)]
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[cfg(unix)]
#[test]
fn e2e_install_verifies_runs_and_rolls_back_a_failed_batch() {
    let hello = script_tarball("hello-1.0.0", "hello", "hello-1");
    let other = script_tarball("other", "other", "other-1");
    let mut routes = HashMap::new();
    routes.insert("/hello.tar.gz".to_string(), hello.clone());
    routes.insert("/other.tar.gz".to_string(), other.clone());
    let base = serve(routes);
    let sb = Sandbox::new("install");

    sb.add(
        "hello",
        "1.0.0",
        &format!("{base}/hello.tar.gz"),
        &sha256(&hello),
    );
    sb.ok(&["install", "hello", "--backend", "custom"]);
    assert_eq!(sb.exec("hello"), "hello-1");
    assert_eq!(sb.installed_version("hello").as_deref(), Some("1.0.0"));

    // A keg maca's ledger knows nothing about (another data dir sharing this
    // prefix, or something the user put there) must survive every rollback.
    let foreign = sb.prefix().join("cellar/foreign/1.0");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("keep"), b"not maca's").unwrap();

    // One good package and one whose checksum is wrong: the batch fails and
    // the good one must be rolled back too (keg, bin link and ledger).
    sb.add(
        "other",
        "1.0.0",
        &format!("{base}/other.tar.gz"),
        &sha256(&other),
    );
    sb.add(
        "broken",
        "1.0.0",
        &format!("{base}/other.tar.gz"),
        &"0".repeat(64),
    );
    let text = sb.fails(&[
        "install",
        "other",
        "broken",
        "--backend",
        "custom",
        "--jobs",
        "1",
    ]);
    assert!(text.contains("checksum mismatch"), "{text}");
    assert!(text.contains("rolled back"), "{text}");
    assert_eq!(sb.installed_version("other"), None);
    assert_eq!(sb.installed_version("broken"), None);
    assert!(!sb.prefix().join("cellar/other").exists());
    assert!(!sb.prefix().join("cellar/broken").exists());
    assert!(sb.prefix().join("bin/other").symlink_metadata().is_err());
    // The package that was there before is untouched.
    assert_eq!(sb.exec("hello"), "hello-1");
    assert_eq!(std::fs::read(foreign.join("keep")).unwrap(), b"not maca's");

    // A missing artifact is a failure, not a silent success.
    sb.add(
        "ghost",
        "1.0.0",
        &format!("{base}/missing.tar.gz"),
        &"0".repeat(64),
    );
    sb.fails(&["install", "ghost", "--backend", "custom"]);

    // Names that would escape the cellar are refused at registration.
    sb.fails(&[
        "custom",
        "add",
        "--name",
        "../evil",
        "--version",
        "1.0.0",
        "--url",
        &format!("{base}/hello.tar.gz"),
    ]);
}

#[cfg(unix)]
#[test]
fn e2e_corrupt_pins_and_failed_lookups_are_errors() {
    let v1 = script_tarball("hello-1.0.0", "hello", "hello-1");
    let mut routes = HashMap::new();
    routes.insert("/hello-1.tar.gz".to_string(), v1.clone());
    let base = serve(routes);
    let sb = Sandbox::new("pinserr");
    sb.add(
        "hello",
        "1.0.0",
        &format!("{base}/hello-1.tar.gz"),
        &sha256(&v1),
    );
    sb.ok(&["install", "hello", "--backend", "custom", "--kind", "cli"]);

    // A package the registry no longer knows cannot be checked: outdated must
    // not claim everything is up to date.
    sb.ok(&["custom", "remove", "hello"]);
    let out = sb.fails(&["outdated"]);
    assert!(!out.contains("all packages are up to date"), "{out}");
    assert!(out.contains("could not check 1 package(s): hello"), "{out}");

    // A corrupt pins file is an error, never "no pins".
    std::fs::write(sb.home.join("pins.json"), "not json").unwrap();
    sb.fails(&["list"]);
    sb.fails(&["outdated"]);
}

#[cfg(unix)]
#[test]
fn e2e_pins_upgrade_lock_sync_and_snapshot_restore() {
    let v1 = script_tarball("hello-1.0.0", "hello", "hello-1");
    let v2 = script_tarball("hello-2.0.0", "hello", "hello-2");
    let mut routes = HashMap::new();
    routes.insert("/hello-1.tar.gz".to_string(), v1.clone());
    routes.insert("/hello-2.tar.gz".to_string(), v2.clone());
    let base = serve(routes);
    let sb = Sandbox::new("lifecycle");

    sb.add(
        "hello",
        "1.0.0",
        &format!("{base}/hello-1.tar.gz"),
        &sha256(&v1),
    );
    sb.ok(&["install", "hello", "--backend", "custom", "--kind", "cli"]);
    sb.ok(&["lock"]);

    // A pin holds the installed version; it cannot name another one.
    sb.fails(&["pin", "hello", "9.9.9"]);
    sb.ok(&["pin", "hello"]);

    // The registry moves to 2.0.0. While pinned, upgrade leaves hello alone.
    sb.add(
        "hello",
        "2.0.0",
        &format!("{base}/hello-2.tar.gz"),
        &sha256(&v2),
    );
    // `outdated` sees the newer version (offline: the registry is local).
    let out = sb.ok(&["outdated", "--json"]);
    let json_start = out.find('[').expect("outdated prints a JSON array");
    let outdated: serde_json::Value = serde_json::Deserializer::from_str(&out[json_start..])
        .into_iter()
        .next()
        .unwrap()
        .unwrap();
    let entry = outdated
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "hello")
        .unwrap_or_else(|| panic!("hello missing from outdated: {out}"));
    assert!(entry.to_string().contains("2.0.0"), "{entry}");
    let text = sb.ok(&["upgrade"]);
    assert!(text.contains("pinned"), "{text}");
    assert_eq!(sb.installed_version("hello").as_deref(), Some("1.0.0"));

    // A corrupt pins file stops the upgrade instead of releasing every pin.
    let pins = sb.home.join("pins.json");
    let good_pins = std::fs::read(&pins).unwrap();
    std::fs::write(&pins, b"{ not json").unwrap();
    sb.fails(&["upgrade"]);
    assert_eq!(sb.installed_version("hello").as_deref(), Some("1.0.0"));
    std::fs::write(&pins, good_pins).unwrap();

    // Unpinned, upgrade moves to 2.0.0 and reclaims the old keg.
    sb.ok(&["unpin", "hello"]);
    sb.ok(&["upgrade"]);
    assert_eq!(sb.installed_version("hello").as_deref(), Some("2.0.0"));
    assert_eq!(sb.exec("hello"), "hello-2");
    assert!(!sb.prefix().join("cellar/hello/1.0.0").exists());
    assert!(sb.prefix().join("cellar/hello/2.0.0").exists());

    // The lock still pins 1.0.0. The registry now serves 2.0.0 but kept 1.0.0
    // as history, so sync replays the lock: it installs 1.0.0 again.
    let text = sb.ok(&["sync"]);
    assert!(text.contains("hello 1.0.0"), "{text}");
    assert_eq!(sb.installed_version("hello").as_deref(), Some("1.0.0"));
    assert_eq!(sb.exec("hello"), "hello-1");
    sb.ok(&["upgrade"]);
    assert_eq!(sb.installed_version("hello").as_deref(), Some("2.0.0"));

    // A lock naming a version the registry never held fails loudly and
    // leaves the installed state as it was.
    let lock_path = sb.prefix().join("maca.lock.json");
    let good_lock = std::fs::read_to_string(&lock_path).unwrap();
    std::fs::write(&lock_path, good_lock.replace("1.0.0", "0.0.1")).unwrap();
    let text = sb.fails(&["sync"]);
    assert!(text.contains("no version 0.0.1"), "{text}");
    assert_eq!(sb.installed_version("hello").as_deref(), Some("2.0.0"));
    assert_eq!(sb.exec("hello"), "hello-2");

    // Re-lock at 2.0.0: sync is now a no-op success and keeps the kind.
    sb.ok(&["lock"]);
    let text = sb.ok(&["sync"]);
    assert!(text.contains("already installed"), "{text}");
    let lock = std::fs::read_to_string(sb.prefix().join("maca.lock.json")).unwrap();
    assert!(lock.contains("\"kind\": \"cli\""), "{lock}");

    // Remove, then restore the automatic pre-remove snapshot.
    sb.ok(&["remove", "hello"]);
    assert_eq!(sb.installed_version("hello"), None);
    assert!(!sb.prefix().join("cellar/hello").exists());
    assert!(sb.prefix().join("bin/hello").symlink_metadata().is_err());
    let id = sb.snapshot_with_reason("before remove");
    sb.ok(&["snapshot", "restore", &id]);
    assert_eq!(sb.installed_version("hello").as_deref(), Some("2.0.0"));
    assert_eq!(sb.exec("hello"), "hello-2");

    // Sync from a clean slate installs what the lock names.
    sb.ok(&["remove", "hello", "--no-snapshot"]);
    sb.ok(&["sync"]);
    assert_eq!(sb.installed_version("hello").as_deref(), Some("2.0.0"));
}

#[cfg(unix)]
#[test]
fn e2e_mirror_only_package_is_found_and_installed() {
    // One loopback server holds the artifact, a second serves the registry
    // JSON that points at it.
    let tool = script_tarball("tool", "mirrored", "from-mirror");
    let artifact_base = {
        let mut routes = HashMap::new();
        routes.insert("/tool.tar.gz".to_string(), tool.clone());
        serve(routes)
    };
    let mut artifacts = serde_json::Map::new();
    for key in ["macos", "linux"] {
        artifacts.insert(
            key.to_string(),
            serde_json::json!({
                "url": format!("{artifact_base}/tool.tar.gz"),
                "sha256": sha256(&tool),
                "kind": "tar.gz",
            }),
        );
    }
    let registry = serde_json::json!({
        "packages": [
            {"name": "mirrored", "version": "3.1.4", "artifacts": artifacts.clone()},
            {"name": "../escape", "version": "1.0.0", "artifacts": artifacts},
        ]
    });
    let mirror_base = {
        let mut routes = HashMap::new();
        routes.insert(
            "/registry.json".to_string(),
            registry.to_string().into_bytes(),
        );
        serve(routes)
    };

    let sb = Sandbox::new("mirror");
    std::fs::write(
        sb.home.join("config.toml"),
        format!("custom_registry_urls = [\"{mirror_base}/registry.json\"]\n"),
    )
    .unwrap();

    // Not in the local registry, only on the mirror.
    assert!(!sb.home.join("custom-registry.json").exists());
    let text = sb.ok(&["search", "mirrored", "--backend", "custom"]);
    assert!(text.contains("mirrored"), "{text}");
    sb.ok(&["install", "mirrored", "--backend", "custom"]);
    assert_eq!(sb.exec("mirrored"), "from-mirror");
    assert_eq!(sb.installed_version("mirrored").as_deref(), Some("3.1.4"));

    // A mirror entry whose name would escape the cellar is refused.
    let text = sb.fails(&["install", "../escape", "--backend", "custom"]);
    assert!(text.contains("not a valid name"), "{text}");
    assert!(!sb.home.join("escape").exists());

    // A plain-http mirror that is not loopback is refused outright.
    std::fs::write(
        sb.home.join("config.toml"),
        "custom_registry_urls = [\"http://example.invalid/registry.json\"]\n",
    )
    .unwrap();
    let text = sb.fails(&["search", "mirrored", "--backend", "custom"]);
    assert!(text.contains("must use https"), "{text}");
}

#[cfg(unix)]
#[test]
fn e2e_binary_kind_build_steps_and_groups() {
    // A source tree whose "build" is a script that installs into the prefix.
    let source = {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        let files: [(&str, &str); 2] = [
            (
                "built-src/install.sh",
                "#!/bin/sh\nset -e\nmkdir -p \"$1/bin\"\ncp tool.sh \"$1/bin/built\"\nchmod 755 \"$1/bin/built\"\n",
            ),
            ("built-src/tool.sh", "#!/bin/sh\necho built-from-steps\n"),
        ];
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, path, body.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    };
    let single = b"#!/bin/sh\necho single-binary\n".to_vec();
    let mut routes = HashMap::new();
    routes.insert("/src.tar.gz".to_string(), source.clone());
    routes.insert("/single".to_string(), single.clone());
    let base = serve(routes);
    let sb = Sandbox::new("kinds");

    // Single-binary artifact: written into the keg, made executable, linked.
    sb.ok(&[
        "custom",
        "add",
        "--name",
        "single",
        "--version",
        "0.1.0",
        "--kind",
        "binary",
        "--url",
        &format!("{base}/single"),
        "--sha256",
        &sha256(&single),
    ]);
    sb.ok(&["install", "single", "--backend", "custom"]);
    assert_eq!(sb.exec("single"), "single-binary");

    // Source artifact with build steps; `{{prefix}}` is the keg.
    sb.ok(&[
        "custom",
        "add",
        "--name",
        "built",
        "--version",
        "2.0.0",
        "--url",
        &format!("{base}/src.tar.gz"),
        "--sha256",
        &sha256(&source),
        "--build-step",
        "sh install.sh {{prefix}}",
    ]);
    sb.ok(&["install", "built", "--backend", "custom"]);
    assert_eq!(sb.exec("built"), "built-from-steps");
    assert!(sb.prefix().join("cellar/built/2.0.0/bin/built").is_file());

    // A failing build step fails the install and leaves no keg, even with
    // the automatic snapshot (and so the rollback) turned off.
    sb.ok(&[
        "custom",
        "add",
        "--name",
        "wontbuild",
        "--version",
        "1.0.0",
        "--url",
        &format!("{base}/src.tar.gz"),
        "--sha256",
        &sha256(&source),
        "--build-step",
        "sh -c 'exit 3'",
    ]);
    let text = sb.fails(&[
        "install",
        "wontbuild",
        "--backend",
        "custom",
        "--no-snapshot",
    ]);
    assert!(text.contains("build step failed"), "{text}");
    assert!(!sb.prefix().join("cellar/wontbuild").exists());
    assert_eq!(sb.installed_version("wontbuild"), None);

    // Auto backend finds a custom package first, with no network needed.
    sb.ok(&["remove", "single"]);
    sb.ok(&["install", "single"]);
    assert_eq!(sb.exec("single"), "single-binary");

    // Groups: a member whose version no longer matches fails the command.
    sb.ok(&["group", "create", "dev"]);
    sb.ok(&[
        "group",
        "add",
        "dev",
        "single",
        "--backend",
        "custom",
        "--kind",
        "cli",
    ]);
    sb.ok(&["group", "install", "dev"]);
    sb.ok(&[
        "group",
        "add",
        "dev",
        "built",
        "--backend",
        "custom",
        "--version",
        "9.9.9",
    ]);
    let text = sb.fails(&["group", "install", "dev"]);
    assert!(
        text.contains("no version 9.9.9") || text.contains("lock pins 9.9.9"),
        "{text}"
    );
    let group = std::fs::read_to_string(sb.home.join("groups/dev.json")).unwrap();
    assert!(
        group.contains("\"single\"") && group.contains("\"built\""),
        "{group}"
    );
}

#[cfg(unix)]
#[test]
fn e2e_cask_zip_install() {
    use std::io::Write;
    let script = b"#!/bin/sh\necho cask-bin\n";
    // Build a zip archive in memory.
    let mut zip_buffer = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut zip_buffer);
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o755);
        zip.start_file("cask/tool.sh", options).unwrap();
        zip.write_all(script).unwrap();
        zip.finish().unwrap();
    }
    let zip_bytes = zip_buffer.into_inner();

    let mut routes = HashMap::new();
    // serve the zip at /tool.zip
    routes.insert("/tool.zip".to_string(), zip_bytes.clone());
    let base = serve(routes);
    // fix cask url
    let cask_json = serde_json::json!({
        "token": "casktool",
        "version": "1.2.3",
        "url": format!("{}/tool.zip", base),
        "sha256": "no_check",
        "artifacts": [
            {"binary": ["tool.sh"], "target": "$HOMEBREW_PREFIX/bin/casktool"}
        ]
    });
    // We need a second server for the cask JSON metadata. Since serve() takes one map, and our map above
    // didn't include /cask/casktool.json, let's hack by starting a new `serve` with both.
    let mut routes = HashMap::new();
    routes.insert("/tool.zip".to_string(), zip_bytes.clone());
    routes.insert(
        "/cask/casktool.json".to_string(),
        cask_json.to_string().into_bytes(),
    );
    let base = serve(routes);
    let cask_api = format!("{}/cask", base);

    let sb = Sandbox::new("caskzip");
    let (success, text) = sb.run_env(
        &["install", "cask:casktool", "--backend", "brew"],
        &[("MACA_CASK_API", &cask_api)],
    );
    assert!(success, "install failed:\n{text}");
    assert_eq!(sb.exec("casktool"), "cask-bin");
}

/// A loopback cask API serving `token` with the given archive and sha256.
#[cfg(unix)]
fn cask_api(token: &str, version: &str, zip_bytes: Vec<u8>, sha: &str) -> String {
    let mut routes = HashMap::new();
    routes.insert("/tool.zip".to_string(), zip_bytes);
    let base = serve(routes.clone());
    // The JSON names the archive URL, so it needs the archive server's origin.
    let body = serde_json::json!({
        "token": token,
        "version": version,
        "url": format!("{base}/tool.zip"),
        "sha256": sha,
        "artifacts": [{"binary": ["tool.sh"], "target": format!("$HOMEBREW_PREFIX/bin/{token}")}]
    });
    let mut api = HashMap::new();
    api.insert(format!("/cask/{token}.json"), body.to_string().into_bytes());
    format!("{}/cask", serve(api))
}

#[cfg(unix)]
fn cask_zip() -> Vec<u8> {
    use std::io::Write;
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
        zip.start_file("tool.sh", opts).unwrap();
        zip.write_all(b"#!/bin/sh\necho cask-bin\n").unwrap();
        zip.finish().unwrap();
    }
    cursor.into_inner()
}

#[cfg(unix)]
#[test]
fn e2e_cask_refuses_a_bad_checksum_a_wrong_version_and_leaves_nothing() {
    let zip = cask_zip();
    let sb = Sandbox::new("caskbad");

    // A declared checksum that does not match: refused, no keg, no ledger entry.
    let api = cask_api("badsum", "1.0.0", zip.clone(), &"0".repeat(64));
    let (ok, text) = sb.run_env(
        &["install", "cask:badsum", "--backend", "brew"],
        &[("MACA_CASK_API", &api)],
    );
    assert!(!ok, "a checksum mismatch must fail:\n{text}");
    assert!(!sb.prefix().join("cellar/badsum").exists(), "{text}");
    assert_eq!(sb.installed_version("badsum"), None);

    // A version other than the catalog's is refused, not silently ignored.
    let api = cask_api("pinned", "1.0.0", zip.clone(), &sha256(&zip));
    let (ok, text) = sb.run_env(
        &[
            "install",
            "cask:pinned",
            "--backend",
            "brew",
            "--version",
            "0.9.0",
        ],
        &[("MACA_CASK_API", &api)],
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("only publishes its current version"),
        "{text}"
    );
    assert_eq!(sb.installed_version("pinned"), None);

    // The matching version installs and the checksum passes.
    let (ok, text) = sb.run_env(
        &[
            "install",
            "cask:pinned",
            "--backend",
            "brew",
            "--version",
            "1.0.0",
        ],
        &[("MACA_CASK_API", &api)],
    );
    assert!(ok, "{text}");
    assert_eq!(sb.exec("pinned"), "cask-bin");
}

/// The custom registry keeps earlier versions, so `--version` can install one,
/// and refuses a version it never held.
#[cfg(unix)]
#[test]
fn e2e_custom_serves_a_registered_older_version_and_refuses_unknown_ones() {
    let a1 = script_tarball("a", "alpha", "alpha-1");
    let a2 = script_tarball("a", "alpha", "alpha-2");
    let mut routes = HashMap::new();
    routes.insert("/a1".to_string(), a1.clone());
    routes.insert("/a2".to_string(), a2.clone());
    let base = serve(routes);
    let sb = Sandbox::new("customver");
    sb.add("alpha", "1.0.0", &format!("{base}/a1"), &sha256(&a1));
    sb.add("alpha", "2.0.0", &format!("{base}/a2"), &sha256(&a2));

    let text = sb.fails(&[
        "install",
        "alpha",
        "--backend",
        "custom",
        "--version",
        "0.5.0",
    ]);
    assert!(text.contains("no version 0.5.0"), "{text}");
    assert_eq!(sb.installed_version("alpha"), None);
    assert!(!sb.prefix().join("cellar/alpha").exists());

    // Unversioned installs the current registration.
    sb.ok(&["install", "alpha", "--backend", "custom"]);
    assert_eq!(sb.exec("alpha"), "alpha-2");
    sb.ok(&["remove", "alpha", "--no-snapshot"]);

    // An older registration installs on request, and a lock of it syncs back.
    sb.ok(&[
        "install",
        "alpha",
        "--backend",
        "custom",
        "--version",
        "1.0.0",
    ]);
    assert_eq!(sb.exec("alpha"), "alpha-1");
    assert_eq!(sb.installed_version("alpha").as_deref(), Some("1.0.0"));
    sb.ok(&["lock"]);
    sb.ok(&["remove", "alpha", "--no-snapshot"]);
    sb.ok(&["sync"]);
    assert_eq!(sb.installed_version("alpha").as_deref(), Some("1.0.0"));
    assert_eq!(sb.exec("alpha"), "alpha-1");
}

/// A project file moves the prefix for commands run under it, and an
/// explicit `--prefix` still wins over it.
#[cfg(unix)]
#[test]
fn e2e_project_file_scopes_installs_and_the_flag_overrides_it() {
    let a1 = script_tarball("a", "alpha", "alpha-1");
    let mut routes = HashMap::new();
    routes.insert("/a1".to_string(), a1.clone());
    let base = serve(routes);
    let sb = Sandbox::new("projscope");
    sb.add("alpha", "1.0.0", &format!("{base}/a1"), &sha256(&a1));

    let proj = sb.home.join("proj");
    std::fs::create_dir_all(proj.join("deep")).unwrap();
    std::fs::write(proj.join("maca.toml"), "prefix = \"proj-prefix\"\n").unwrap();

    let out = Command::new(&sb.bin)
        .args(["install", "alpha", "--backend", "custom"])
        .current_dir(proj.join("deep"))
        .env("MACA_HOME", &sb.home)
        .env_remove("MACA_PREFIX")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(proj.join("proj-prefix/cellar/alpha/1.0.0").is_dir());
    assert!(!sb.prefix().join("cellar/alpha").exists());
    let linked = Command::new(proj.join("proj-prefix/bin/alpha"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&linked.stdout).trim(), "alpha-1");

    // `--prefix` beats the project file: shellenv reports the flag's value.
    let flag = sb.home.join("flag-prefix");
    let out = Command::new(&sb.bin)
        .args([
            "--prefix",
            flag.to_str().unwrap(),
            "shellenv",
            "--shell",
            "bash",
        ])
        .current_dir(&proj)
        .env("MACA_HOME", &sb.home)
        .env_remove("MACA_PREFIX")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("flag-prefix"), "{text}");
    assert!(!text.contains("proj-prefix"), "{text}");
}

#[cfg(unix)]
#[test]
fn e2e_prefixes_and_snapshot_scopes_stay_isolated() {
    let a1 = script_tarball("a", "alpha", "alpha-1");
    let a2 = script_tarball("a", "alpha", "alpha-2");
    let b1 = script_tarball("b", "beta", "beta-1");
    let mut routes = HashMap::new();
    routes.insert("/a1".to_string(), a1.clone());
    routes.insert("/a2".to_string(), a2.clone());
    routes.insert("/b1".to_string(), b1.clone());
    let base = serve(routes);
    let sb = Sandbox::new("isolation");
    let other = sb.home.join("other-prefix");
    let other_s = other.to_str().unwrap();

    sb.add("alpha", "1.0.0", &format!("{base}/a1"), &sha256(&a1));
    sb.add("beta", "1.0.0", &format!("{base}/b1"), &sha256(&b1));
    sb.ok(&["install", "alpha", "beta", "--backend", "custom"]);
    // A second prefix sharing this data dir holds a different package.
    sb.add("gamma", "1.0.0", &format!("{base}/b1"), &sha256(&b1));
    sb.ok(&[
        "--prefix",
        other_s,
        "install",
        "gamma",
        "--backend",
        "custom",
    ]);
    let gamma_keg = other.join("cellar/gamma/1.0.0");
    assert!(gamma_keg.is_dir());
    // The ledger records one install per name, so the same package cannot
    // also go into this prefix; that is refused rather than orphaning it.
    let text = sb.fails(&["install", "gamma", "--backend", "custom", "--no-snapshot"]);
    assert!(
        text.contains("already installed in another prefix"),
        "{text}"
    );
    assert!(!sb.prefix().join("cellar/gamma").exists());

    // A foreign symlink in bin/ (not a link into the cellar) must survive
    // any rollback.
    let foreign = sb.prefix().join("bin/mytool");
    std::os::unix::fs::symlink("/usr/bin/true", &foreign).unwrap();

    // An --only snapshot of alpha, then beta is removed and alpha upgraded.
    sb.ok(&[
        "snapshot",
        "create",
        "--note",
        "only alpha",
        "--only",
        "alpha",
    ]);
    let only_id = sb.snapshot_with_reason("only alpha");
    sb.add("alpha", "2.0.0", &format!("{base}/a2"), &sha256(&a2));
    sb.ok(&["upgrade", "--package", "alpha"]);
    assert_eq!(sb.exec("alpha"), "alpha-2");
    sb.ok(&["remove", "beta", "--no-snapshot"]);

    // Restoring it puts alpha 1.0.0 back and leaves everything else alone:
    // beta stays removed, gamma in the other prefix is untouched, the
    // foreign link survives.
    sb.ok(&["snapshot", "restore", &only_id]);
    assert_eq!(sb.exec("alpha"), "alpha-1");
    assert_eq!(sb.installed_version("alpha").as_deref(), Some("1.0.0"));
    assert!(!sb.prefix().join("cellar/alpha/2.0.0").exists());
    assert_eq!(sb.installed_version("beta"), None);
    assert!(!sb.prefix().join("cellar/beta").exists());
    assert_eq!(sb.installed_version("gamma").as_deref(), Some("1.0.0"));
    assert!(gamma_keg.is_dir());
    assert_eq!(
        std::fs::read_link(&foreign).unwrap(),
        PathBuf::from("/usr/bin/true")
    );

    // A failed install in this prefix rolls back without touching the other.
    sb.add("broken", "1.0.0", &format!("{base}/a1"), &"0".repeat(64));
    sb.fails(&["install", "broken", "--backend", "custom"]);
    assert!(gamma_keg.is_dir());
    assert_eq!(sb.installed_version("gamma").as_deref(), Some("1.0.0"));
    assert_eq!(
        std::fs::read_link(&foreign).unwrap(),
        PathBuf::from("/usr/bin/true")
    );

    // `lock` here records only this prefix's packages, and sync --prune
    // prunes nothing in the other prefix.
    sb.ok(&["lock"]);
    let lock = std::fs::read_to_string(sb.prefix().join("maca.lock.json")).unwrap();
    assert!(!lock.contains("gamma"), "{lock}");
    sb.ok(&["sync", "--prune"]);
    assert!(gamma_keg.is_dir());
    assert_eq!(sb.installed_version("gamma").as_deref(), Some("1.0.0"));
    assert_eq!(sb.exec("alpha"), "alpha-1");

    // Removing a package that lives in another prefix is refused.
    let text = sb.fails(&["remove", "gamma", "--no-snapshot"]);
    assert!(text.contains("refusing to remove"), "{text}");
    assert!(gamma_keg.is_dir());

    // The same name twice in one command installs once (the registry now
    // serves 2.0.0).
    sb.ok(&["remove", "alpha", "--no-snapshot"]);
    sb.ok(&["install", "alpha", "alpha", "--backend", "custom"]);
    assert_eq!(sb.exec("alpha"), "alpha-2");
}

/// Dry-run a GitHub release against a loopback API. The printed URL is the
/// host asset from the fixture, not a reimplementation of the picker.
#[cfg(unix)]
#[test]
fn e2e_github_dry_run_prints_host_asset() {
    let asset = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "hello-1.2.3-aarch64-apple-darwin.tar.gz"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "hello-1.2.3-x86_64-apple-darwin.tar.gz"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "hello-1.2.3-aarch64-unknown-linux-gnu.tar.gz"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "hello-1.2.3-x86_64-unknown-linux-gnu.tar.gz"
    } else if cfg!(target_os = "windows") {
        "hello-1.2.3-x86_64-pc-windows-msvc.zip"
    } else {
        panic!("this target has no fixture asset");
    };
    let names = [
        "hello-1.2.3-aarch64-apple-darwin.tar.gz",
        "hello-1.2.3-x86_64-apple-darwin.tar.gz",
        "hello-1.2.3-aarch64-unknown-linux-gnu.tar.gz",
        "hello-1.2.3-x86_64-unknown-linux-gnu.tar.gz",
        "hello-1.2.3-x86_64-pc-windows-msvc.zip",
        "checksums.txt",
    ];
    let assets: Vec<serde_json::Value> = names
        .iter()
        .map(|name| {
            serde_json::json!({
                "name": name,
                "browser_download_url": format!("https://example.test/{name}"),
            })
        })
        .collect();
    let body = serde_json::to_vec(&serde_json::json!({
        "tag_name": "v1.2.3",
        "assets": assets,
    }))
    .unwrap();
    let mut routes = HashMap::new();
    routes.insert("/repos/acme/hello/releases/latest".into(), body.clone());
    // An older release, published under the tag spelling with no leading v.
    let older = serde_json::to_vec(&serde_json::json!({
        "tag_name": "1.0.0",
        "assets": [{
            "name": asset,
            "browser_download_url": format!("https://example.test/old/{asset}"),
        }],
    }))
    .unwrap();
    routes.insert(
        "/repos/acme/hello/releases/tags/v1.0.0".into(),
        older.clone(),
    );
    routes.insert("/repos/acme/hello/releases/tags/1.0.0".into(), older);
    let base = serve(routes);
    let sb = Sandbox::new("github-plan");
    let (ok, text) = sb.run_env(
        &[
            "install",
            "acme/hello",
            "--backend",
            "github",
            "--dry-run",
            "--no-snapshot",
        ],
        &[("MACA_GITHUB_API", base.as_str())],
    );
    assert!(ok, "{text}");
    assert!(text.contains("1.2.3"), "{text}");
    assert!(
        text.contains(&format!("https://example.test/{asset}")),
        "{text}"
    );

    // The version is requested, so the current release must not be used.
    let (ok, text) = sb.run_env(
        &[
            "install",
            "acme/hello",
            "--backend",
            "github",
            "--version",
            "1.0.0",
            "--dry-run",
            "--no-snapshot",
        ],
        &[("MACA_GITHUB_API", base.as_str())],
    );
    assert!(ok, "{text}");
    assert!(text.contains("https://example.test/old/"), "{text}");
    assert!(text.contains("1.0.0"), "{text}");

    // A requested version with no backend named has no catalog to fetch it
    // from, so it fails before anything is installed.
    let (ok, text) = sb.run_env(
        &[
            "install",
            "acme/hello",
            "--version",
            "1.0.0",
            "--no-snapshot",
        ],
        &[("MACA_GITHUB_API", base.as_str())],
    );
    assert!(!ok, "{text}");
    assert!(text.contains("needs an explicit backend"), "{text}");
}

/// A Homebrew formula JSON fixture with a bottle for the test host.
#[cfg(unix)]
fn formula_json(name: &str, version: &str, bottle_url: &str) -> Vec<u8> {
    let keys: &[&str] = if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            &[
                "arm64_sequoia",
                "arm64_sonoma",
                "arm64_ventura",
                "arm64_monterey",
            ]
        } else {
            &["sequoia", "sonoma", "ventura", "monterey"]
        }
    } else if cfg!(target_arch = "aarch64") {
        &["arm64_linux"]
    } else {
        &["x86_64_linux"]
    };
    let mut files = serde_json::Map::new();
    for key in keys {
        files.insert(
            (*key).to_string(),
            serde_json::json!({ "url": bottle_url, "sha256": "00".repeat(32) }),
        );
    }
    serde_json::to_vec(&serde_json::json!({
        "name": name,
        "desc": "test formula",
        "homepage": "https://example.test",
        "versions": { "stable": version },
        "urls": { "stable": { "url": bottle_url } },
        "executables": [],
        "bottle": { "stable": { "files": files } },
        "dependencies": [],
        "uses_from_macos": [],
        "disabled": false
    }))
    .unwrap()
}

/// A named brew version is served by a versioned formula (`python@3.12`) when
/// the current one does not satisfy it, and a version no formula publishes is
/// refused rather than rounded to a nearby one.
#[cfg(unix)]
#[test]
fn e2e_brew_resolves_a_versioned_formula_and_refuses_unknown_ones() {
    let mut routes = HashMap::new();
    routes.insert(
        "/python.json".to_string(),
        formula_json(
            "python",
            "3.13.0",
            "https://example.test/python-3.13.0.tar.gz",
        ),
    );
    routes.insert(
        "/python@3.12.json".to_string(),
        formula_json(
            "python@3.12",
            "3.12.4",
            "https://example.test/python@3.12-3.12.4.tar.gz",
        ),
    );
    let base = serve(routes);
    let sb = Sandbox::new("brew-version");
    let env = [("MACA_FORMULA_API", base.as_str())];

    // The current formula satisfies a prefix of its own version.
    let (ok, text) = sb.run_env(
        &[
            "install",
            "python",
            "--backend",
            "brew",
            "--version",
            "3.13",
            "--dry-run",
            "--no-snapshot",
        ],
        &env,
    );
    assert!(ok, "{text}");
    assert!(text.contains("python 3.13.0"), "{text}");

    // A version only a versioned formula publishes resolves to it.
    let (ok, text) = sb.run_env(
        &[
            "install",
            "python",
            "--backend",
            "brew",
            "--version",
            "3.12",
            "--dry-run",
            "--no-snapshot",
        ],
        &env,
    );
    assert!(ok, "{text}");
    assert!(text.contains("python@3.12 3.12.4"), "{text}");

    // A version nothing publishes is refused, not rounded.
    let (ok, text) = sb.run_env(
        &[
            "install",
            "python",
            "--backend",
            "brew",
            "--version",
            "3.11",
            "--dry-run",
            "--no-snapshot",
        ],
        &env,
    );
    assert!(!ok, "{text}");
    assert!(text.contains("no versioned formula for 3.11"), "{text}");
}

#[cfg(unix)]
fn winget_manifest(id: &str, version: &str, url: &str) -> Vec<u8> {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    format!(
        "PackageIdentifier: {id}\nPackageVersion: {version}\nInstallers:\n  - Architecture: {arch}\n    InstallerUrl: {url}\n    InstallerSha256: {}\n",
        "00".repeat(32)
    )
    .into_bytes()
}

/// A named winget version is served by an older manifest directory, and a
/// version winget-pkgs does not publish is refused.
#[cfg(unix)]
#[test]
fn e2e_winget_resolves_an_older_manifest_and_refuses_unknown_ones() {
    let mut routes = HashMap::new();
    routes.insert(
        "/a/acme/hello".to_string(),
        serde_json::to_vec(&serde_json::json!([
            { "name": "1.0.0", "type": "dir" },
            { "name": "2.0.0", "type": "dir" },
        ]))
        .unwrap(),
    );
    routes.insert(
        "/a/acme/hello/1.0.0/acme.hello.installer.yaml".to_string(),
        winget_manifest(
            "acme.hello",
            "1.0.0",
            "https://example.test/hello-1.0.0.exe",
        ),
    );
    routes.insert(
        "/a/acme/hello/2.0.0/acme.hello.installer.yaml".to_string(),
        winget_manifest(
            "acme.hello",
            "2.0.0",
            "https://example.test/hello-2.0.0.exe",
        ),
    );
    let base = serve(routes);
    let sb = Sandbox::new("winget-version");
    let env = [
        ("MACA_WINGET_CONTENTS_API", base.as_str()),
        ("MACA_WINGET_PKGS_RAW", base.as_str()),
    ];

    // A published older version is fetched by name.
    let (ok, text) = sb.run_env(
        &[
            "install",
            "acme.hello",
            "--backend",
            "winget",
            "--version",
            "1.0.0",
            "--dry-run",
            "--no-snapshot",
        ],
        &env,
    );
    assert!(ok, "{text}");
    assert!(text.contains("acme.hello 1.0.0"), "{text}");
    assert!(text.contains("hello-1.0.0.exe"), "{text}");

    // Without a request the newest published version wins.
    let (ok, text) = sb.run_env(
        &[
            "install",
            "acme.hello",
            "--backend",
            "winget",
            "--dry-run",
            "--no-snapshot",
        ],
        &env,
    );
    assert!(ok, "{text}");
    assert!(text.contains("acme.hello 2.0.0"), "{text}");

    // A version winget-pkgs never published is refused.
    let (ok, text) = sb.run_env(
        &[
            "install",
            "acme.hello",
            "--backend",
            "winget",
            "--version",
            "1.5",
            "--dry-run",
            "--no-snapshot",
        ],
        &env,
    );
    assert!(!ok, "{text}");
    assert!(text.contains("no version 1.5"), "{text}");
}

/// Serve a github release whose host asset is `archive`, and install it.
#[cfg(unix)]
fn github_install(sb: &Sandbox, archive: Vec<u8>) -> (bool, String) {
    let asset = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "flat-1.0.0-aarch64-apple-darwin.tar.gz"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "flat-1.0.0-x86_64-apple-darwin.tar.gz"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "flat-1.0.0-aarch64-unknown-linux-gnu.tar.gz"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "flat-1.0.0-x86_64-unknown-linux-gnu.tar.gz"
    } else {
        panic!("this target has no fixture asset");
    };
    let mut routes = HashMap::new();
    routes.insert(format!("/dl/{asset}"), archive);
    let base = serve(routes.clone());
    let release = serde_json::to_vec(&serde_json::json!({
        "tag_name": "v1.0.0",
        "assets": [{
            "name": asset,
            "browser_download_url": format!("{base}/dl/{asset}"),
        }],
    }))
    .unwrap();
    routes.insert("/repos/acme/flat/releases/latest".into(), release);
    let base = serve(routes);
    sb.run_env(
        &[
            "install",
            "acme/flat",
            "--backend",
            "github",
            "--no-snapshot",
        ],
        &[("MACA_GITHUB_API", base.as_str())],
    )
}

#[cfg(unix)]
#[test]
fn e2e_github_links_executables_at_the_archive_root() {
    let sb = Sandbox::new("github-flat");
    let archive = tarball(&[
        ("flat-1.0.0/flatexe", 0o755, "#!/bin/sh\necho flat-ok\n"),
        ("flat-1.0.0/README", 0o644, "docs\n"),
    ]);
    let (ok, text) = github_install(&sb, archive);
    assert!(ok, "{text}");
    assert_eq!(sb.exec("flatexe"), "flat-ok");
    assert!(!sb.prefix().join("bin").join("README").exists());
}

#[cfg(unix)]
#[test]
fn e2e_github_bin_layout_still_links_from_bin() {
    let sb = Sandbox::new("github-binlayout");
    let archive = script_tarball("flat-1.0.0", "binexe", "bin-ok");
    let (ok, text) = github_install(&sb, archive);
    assert!(ok, "{text}");
    assert_eq!(sb.exec("binexe"), "bin-ok");
}

#[cfg(unix)]
#[test]
fn e2e_github_archive_without_executables_fails_and_leaves_nothing() {
    let sb = Sandbox::new("github-noexe");
    let archive = tarball(&[
        ("flat-1.0.0/README", 0o644, "docs\n"),
        ("flat-1.0.0/LICENSE", 0o644, "mit\n"),
    ]);
    let (ok, text) = github_install(&sb, archive);
    assert!(!ok, "{text}");
    assert!(text.contains("no executables"), "{text}");
    assert_eq!(sb.installed_version("acme--flat"), None);
    let keg_parent = sb.prefix().join("cellar").join("acme--flat");
    assert!(
        !keg_parent.join("1.0.0").exists(),
        "keg left behind: {text}"
    );
}

#[cfg(unix)]
#[test]
fn group_names_that_escape_the_groups_dir_are_refused() {
    let sb = Sandbox::new("group-names");
    let text = sb.fails(&["group", "create", "../evil"]);
    assert!(text.contains("not a valid path component"), "{text}");
    assert!(!sb.home.join("evil.json").exists());
    sb.fails(&["group", "delete", "../evil"]);
    sb.ok(&["group", "create", "dev-tools"]);
    assert!(sb.home.join("groups/dev-tools.json").exists());
}

/// `self-update` end to end against a loopback "GitHub": the release list,
/// the archive and its checksum. It runs on a copy of the binary, so the
/// build under test is never replaced.
#[cfg(unix)]
#[test]
fn e2e_self_update_replaces_a_copy_of_the_binary_from_a_prerelease() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new("selfupdate");
    let bin_dir = sb.home.join("copy");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let copy = bin_dir.join("maca");
    std::fs::copy(maca_bin(), &copy).unwrap();
    std::fs::write(bin_dir.join("maca.bak.1"), b"stale backup").unwrap();

    let triple = maca::selfupdate::target_triple();
    let asset = format!("maca-{triple}.tar.gz");
    let archive = tarball(&[("maca", 0o755, "#!/bin/sh\necho replaced-by-update\n")]);
    let digest = sha256(&archive);

    // The asset URLs must point back at the server, whose address is only
    // known once it is listening: bind first, then build the routes.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let releases = serde_json::json!([
        {"tag_name": "v0.0.1", "prerelease": false, "draft": false, "assets": []},
        {"tag_name": "v999.0.0-alpha.1", "prerelease": true, "draft": false, "assets": [
            {"name": asset, "browser_download_url": format!("{base}/dl/{asset}")},
            {"name": format!("{asset}.sha256"), "browser_download_url": format!("{base}/dl/{asset}.sha256")}
        ]},
        {"tag_name": "v9999.0.0", "prerelease": false, "draft": true, "assets": []}
    ]);
    let mut routes = HashMap::new();
    routes.insert(
        "/repos/acme/maca/releases?per_page=100".to_string(),
        serde_json::to_vec(&releases).unwrap(),
    );
    routes.insert(format!("/dl/{asset}"), archive);
    routes.insert(
        format!("/dl/{asset}.sha256"),
        format!("{digest}  {asset}\n").into_bytes(),
    );
    assert_eq!(serve_on(listener, routes), base);

    let run = |args: &[&str]| {
        let out = Command::new(&copy)
            .args(args)
            .env("MACA_HOME", &sb.home)
            .env_remove("MACA_PREFIX")
            .env("MACA_GITHUB_API", &base)
            .env("MACA_UPDATE_REPO", "acme/maca")
            .output()
            .unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };

    let (ok, text) = run(&["self-update", "--check", "--prerelease"]);
    assert!(ok, "{text}");
    assert!(text.contains("-> 999.0.0-alpha.1"), "{text}");

    let (ok, text) = run(&["self-update", "--prerelease"]);
    assert!(ok, "{text}");
    assert!(text.contains("updated to v999.0.0-alpha.1"), "{text}");

    // The copy is now the script from the archive, and still executable.
    let mode = std::fs::metadata(&copy).unwrap().permissions().mode();
    assert_ne!(mode & 0o111, 0, "replacement is not executable");
    let out = Command::new(&copy).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "replaced-by-update"
    );
    // One backup of the previous binary is left; the stale one is pruned.
    let backups: Vec<_> = std::fs::read_dir(&bin_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("maca.bak."))
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_ne!(backups[0], "maca.bak.1");
    assert_eq!(
        std::fs::read(bin_dir.join(&backups[0])).unwrap(),
        std::fs::read(maca_bin()).unwrap(),
        "the backup is not the previous binary"
    );
}
