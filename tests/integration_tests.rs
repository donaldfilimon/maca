use std::path::{Path, PathBuf};
use std::process::Command;

fn maca_bin() -> PathBuf {
    // Cargo builds the binary for integration tests and exports its path.
    PathBuf::from(env!("CARGO_BIN_EXE_maca"))
}

/// A `maca` command confined to a throwaway data dir and prefix, so the suite
/// never reads or writes the developer's real `~/.maca`.
fn maca_cmd(bin: &Path) -> Command {
    let home = std::env::temp_dir().join(format!("maca-it-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(bin);
    cmd.env("MACA_HOME", &home).env_remove("MACA_PREFIX");
    cmd
}

#[test]
fn test_version_and_help() {
    let bin = maca_bin();
    if !bin.exists() {
        return;
    }
    let output = maca_cmd(&bin).arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("maca 0.1.0"));

    let help = maca_cmd(&bin).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help_out = String::from_utf8_lossy(&help.stdout);
    assert!(help_out.contains("shellenv"));
    assert!(help_out.contains("completions"));
    assert!(help_out.contains("outdated"));
    assert!(help_out.contains("clean"));
}

#[test]
fn test_shellenv_formats() {
    let bin = maca_bin();
    if !bin.exists() {
        return;
    }
    // Test default bash/zsh format
    let out = maca_cmd(&bin).arg("shellenv").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("export MACA_PREFIX="));
    assert!(text.contains("export PATH="));
    assert!(text.contains("export MANPATH="));

    // Test fish format
    let fish_out = maca_cmd(&bin)
        .args(["shellenv", "--shell", "fish"])
        .output()
        .unwrap();
    assert!(fish_out.status.success());
    let fish_text = String::from_utf8_lossy(&fish_out.stdout);
    assert!(fish_text.contains("set -gx MACA_PREFIX"));
    assert!(fish_text.contains("fish_add_path"));
}

#[test]
fn test_completions() {
    let bin = maca_bin();
    if !bin.exists() {
        return;
    }
    for shell in ["bash", "zsh", "fish"] {
        let out = maca_cmd(&bin)
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(out.status.success(), "completions for {shell} failed");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(!text.trim().is_empty());
        assert!(text.contains("search") || text.contains("install"));
    }
}

#[test]
fn test_outdated_command() {
    let bin = maca_bin();
    if !bin.exists() {
        return;
    }
    let out = maca_cmd(&bin)
        .args(["outdated", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(val.is_array());
}

#[test]
fn test_clean_command() {
    let bin = maca_bin();
    if !bin.exists() {
        return;
    }
    let out = maca_cmd(&bin)
        .args(["clean", "--keep-snapshots", "10"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("clean complete"));
}

#[test]
fn test_doctor_command() {
    let bin = maca_bin();
    if !bin.exists() {
        return;
    }
    let out = maca_cmd(&bin).arg("doctor").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("maca doctor"));
    assert!(text.contains("doctor summary:"));
}

#[test]
fn test_binary_discovery_and_share_linking() {
    let temp = std::env::temp_dir().join(format!("maca-it-bins-{}", std::process::id()));
    let prefix = temp.join("prefix");
    let cellar = temp.join("cellar/demo/1.2.3");
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

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_list_command() {
    let bin = maca_bin();
    if !bin.exists() {
        return;
    }
    let out = maca_cmd(&bin).arg("list").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("PINNED") || text.contains("no packages installed"));
}

#[test]
fn test_sync_dry_run() {
    let bin = maca_bin();
    if !bin.exists() {
        return;
    }
    // Lock current state then dry-run sync
    let lock_out = maca_cmd(&bin).arg("lock").output().unwrap();
    assert!(lock_out.status.success());

    let sync_out = maca_cmd(&bin)
        .args(["sync", "--dry-run", "--prune"])
        .output()
        .unwrap();
    assert!(sync_out.status.success());
    let text = String::from_utf8_lossy(&sync_out.stdout);
    assert!(text.contains("sync complete"));
}

// ---------------------------------------------------------------------------
// Hermetic end-to-end tests: a loopback HTTP server stands in for an artifact
// host, and each test gets its own MACA_HOME.
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;

/// Serve `routes` (path -> body) on 127.0.0.1 for the life of the test process.
fn serve(routes: HashMap<String, Vec<u8>>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
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

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

struct Sandbox {
    bin: PathBuf,
    home: PathBuf,
}

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
        let out = Command::new(&self.bin)
            .args(args)
            .env("MACA_HOME", &self.home)
            .env_remove("MACA_PREFIX")
            .output()
            .unwrap();
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

    // The lock still pins 1.0.0, which the registry no longer serves: sync
    // must fail loudly and leave the installed state as it was.
    let text = sb.fails(&["sync"]);
    assert!(text.contains("lock pins 1.0.0"), "{text}");
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
    assert!(text.contains("lock pins 9.9.9"), "{text}");
    let group = std::fs::read_to_string(sb.home.join("groups/dev.json")).unwrap();
    assert!(
        group.contains("\"single\"") && group.contains("\"built\""),
        "{group}"
    );
}
