//! Source builds: compile a package from its source tarball into the cellar.
//!
//! Used when a formula has no bottle for the host (or `--build-from-source` is
//! passed) and by the custom backend when an entry declares `build_steps`.
//!
//! Build systems are detected from the extracted source tree, in priority
//! order: Autotools (`configure`), CMake, Meson, Make, Cargo, Python. Every
//! build writes a log under `<data>/logs/` and fails loudly on a non-zero
//! command exit.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSystem {
    Autotools,
    CMake,
    Meson,
    Make,
    Cargo,
    Python,
}

impl BuildSystem {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Autotools => "autotools",
            Self::CMake => "cmake",
            Self::Meson => "meson",
            Self::Make => "make",
            Self::Cargo => "cargo",
            Self::Python => "python",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuildResult {
    pub build_system: String,
    pub dest: PathBuf,
    pub log: PathBuf,
}

/// Detect the build system present in an extracted source tree.
pub fn detect(source_dir: &Path) -> Option<BuildSystem> {
    if source_dir.join("configure").exists() {
        return Some(BuildSystem::Autotools);
    }
    if source_dir.join("CMakeLists.txt").exists() {
        return Some(BuildSystem::CMake);
    }
    if source_dir.join("meson.build").exists() {
        return Some(BuildSystem::Meson);
    }
    if source_dir.join("Makefile").exists() || source_dir.join("makefile").exists() {
        return Some(BuildSystem::Make);
    }
    if source_dir.join("Cargo.toml").exists() {
        return Some(BuildSystem::Cargo);
    }
    if source_dir.join("pyproject.toml").exists() || source_dir.join("setup.py").exists() {
        return Some(BuildSystem::Python);
    }
    None
}

/// Build `source_dir` and install into `dest` (the cellar keg directory).
pub fn build(
    system: BuildSystem,
    source_dir: &Path,
    dest: &Path,
    jobs: usize,
) -> Result<BuildResult> {
    let keg = dest
        .parent()
        .and_then(|p| p.file_name())
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let log = log_path(&format!("{keg}-{}", system.name()));
    let mut log_file = std::fs::File::create(&log)?;
    std::fs::create_dir_all(dest)?;
    let job_flag = jobs.to_string();
    let prefix = dest.display().to_string();
    let status = match system {
        BuildSystem::Autotools => {
            run_logged(
                &mut log_file,
                "./configure",
                &[format!("--prefix={prefix}")],
                source_dir,
            )? && run_logged(
                &mut log_file,
                "make",
                &["-j".into(), job_flag.clone()],
                source_dir,
            )? && run_logged(&mut log_file, "make", &["install".into()], source_dir)?
        }
        BuildSystem::CMake => {
            run_logged(
                &mut log_file,
                "cmake",
                &[
                    "-S".into(),
                    ".".into(),
                    "-B".into(),
                    "build".into(),
                    format!("-DCMAKE_INSTALL_PREFIX={prefix}"),
                ],
                source_dir,
            )? && run_logged(
                &mut log_file,
                "cmake",
                &["--build".into(), "build".into()],
                source_dir,
            )? && run_logged(
                &mut log_file,
                "cmake",
                &["--install".into(), "build".into()],
                source_dir,
            )?
        }
        BuildSystem::Meson => {
            run_logged(
                &mut log_file,
                "meson",
                &["setup".into(), "build".into(), format!("--prefix={prefix}")],
                source_dir,
            )? && run_logged(
                &mut log_file,
                "meson",
                &["compile".into(), "-C".into(), "build".into()],
                source_dir,
            )? && run_logged(
                &mut log_file,
                "meson",
                &["install".into(), "-C".into(), "build".into()],
                source_dir,
            )?
        }
        BuildSystem::Make => {
            run_logged(
                &mut log_file,
                "make",
                &["-j".into(), job_flag.clone(), format!("PREFIX={prefix}")],
                source_dir,
            )? && run_logged(
                &mut log_file,
                "make",
                &["install".into(), format!("PREFIX={prefix}")],
                source_dir,
            )?
        }
        BuildSystem::Cargo => run_logged(
            &mut log_file,
            "cargo",
            &[
                "install".into(),
                "--path".into(),
                ".".into(),
                "--root".into(),
                prefix.clone(),
            ],
            source_dir,
        )?,
        BuildSystem::Python => run_logged(
            &mut log_file,
            "python3",
            &[
                "-m".into(),
                "pip".into(),
                "install".into(),
                ".".into(),
                "--prefix".into(),
                prefix.clone(),
            ],
            source_dir,
        )?,
    };
    if !status {
        return Err(Error::Other(anyhow::anyhow!(
            "{:?} build failed; see {}",
            system,
            log.display()
        )));
    }
    Ok(BuildResult {
        build_system: system.name().to_string(),
        dest: dest.to_path_buf(),
        log,
    })
}

/// Run a custom command list (custom backend `build_steps`), stopping on the
/// first failure. Each step is split on whitespace (no shell).
pub fn run_steps(
    source_dir: &Path,
    dest: &Path,
    steps: &[String],
    jobs: usize,
) -> Result<BuildResult> {
    let _ = jobs;
    let name = dest
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let log = log_path(&format!("{name}-custom"));
    let mut log_file = std::fs::File::create(&log)?;
    std::fs::create_dir_all(dest)?;
    for step in steps {
        let parts = split_words(step);
        let Some(program) = parts.first().cloned() else {
            continue;
        };
        let args: Vec<String> = parts[1..]
            .iter()
            .map(|s| s.replace("{{prefix}}", &dest.display().to_string()))
            .collect();
        if !run_logged(&mut log_file, &program, &args, source_dir)? {
            return Err(Error::Other(anyhow::anyhow!(
                "build step failed: {step}; see {}",
                log.display()
            )));
        }
    }
    Ok(BuildResult {
        build_system: "custom".to_string(),
        dest: dest.to_path_buf(),
        log,
    })
}

/// Split a command line into words, honoring single/double quotes and
/// backslash escapes so paths with spaces survive intact.
pub fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut quote: Option<char> = None;
    let mut has = false;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has = true;
                }
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                        has = true;
                    }
                }
                c if c.is_whitespace() => {
                    if has {
                        out.push(std::mem::take(&mut cur));
                        has = false;
                    }
                }
                _ => {
                    cur.push(c);
                    has = true;
                }
            },
        }
    }
    if has {
        out.push(cur);
    }
    out
}

fn run_logged(log: &mut std::fs::File, program: &str, args: &[String], cwd: &Path) -> Result<bool> {
    use std::io::Write;
    writeln!(
        log,
        "+ cd {} && {program} {}",
        cwd.display(),
        args.join(" ")
    )?;
    log.flush()?;
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        // A build must not see the caller's environment. API tokens, a private
        // GOPROXY, or a `CFLAGS` left over from something else would make two
        // builds of the same source differ, and leak into the log.
        .env_clear()
        .envs(build_env())
        .status();
    match status {
        Ok(s) => {
            writeln!(log, "[exit {}]", s.code().unwrap_or(-1))?;
            log.flush()?;
            Ok(s.success())
        }
        Err(e) => {
            writeln!(log, "[spawn error] {e}")?;
            log.flush()?;
            Err(Error::Other(anyhow::anyhow!(
                "could not run {program} in {}: {e}",
                cwd.display()
            )))
        }
    }
}

/// The environment a source build is allowed to inherit.
///
/// Search paths and locales, nothing else. A variable not in this list is
/// dropped even when the caller exported it.
pub fn build_env() -> Vec<(String, String)> {
    let mut env = Vec::new();
    for key in [
        "PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "USER", "LOGNAME",
    ] {
        if let Ok(value) = std::env::var(key)
            && !value.is_empty()
        {
            env.push((key.to_string(), value));
        }
    }
    env.push(("SOURCE_DATE_EPOCH".into(), "0".into()));
    env
}

fn log_path(name: &str) -> PathBuf {
    let dir = crate::config::data_dir().join("logs");
    std::fs::create_dir_all(&dir).ok();
    dir.join(format!("{}.log", name.replace('/', "_")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_inherits_the_search_path_and_nothing_secret() {
        let keys: Vec<String> = build_env().into_iter().map(|(k, _)| k).collect();
        assert!(keys.contains(&"PATH".into()));
        for dropped in ["CFLAGS", "GITHUB_TOKEN", "MACA_GITHUB_TOKEN", "GOPROXY"] {
            assert!(!keys.iter().any(|k| k == dropped), "{dropped} leaked");
        }
        assert!(keys.iter().any(|k| k == "SOURCE_DATE_EPOCH"));
    }

    #[test]
    fn detects_build_systems() {
        let base = std::env::temp_dir().join(format!("maca-src-detect-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("configure"), b"").unwrap();
        assert_eq!(detect(&base), Some(BuildSystem::Autotools));
        std::fs::remove_file(base.join("configure")).unwrap();
        std::fs::write(base.join("CMakeLists.txt"), b"").unwrap();
        assert_eq!(detect(&base), Some(BuildSystem::CMake));
        std::fs::remove_file(base.join("CMakeLists.txt")).unwrap();
        std::fs::write(base.join("Cargo.toml"), b"").unwrap();
        assert_eq!(detect(&base), Some(BuildSystem::Cargo));
        std::fs::remove_file(base.join("Cargo.toml")).unwrap();
        assert_eq!(detect(&base), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn make_build_installs_into_dest() {
        let base = std::env::temp_dir().join(format!("maca-src-make-{}", std::process::id()));
        let src = base.join("src");
        let dest = base.join("dest");
        std::fs::create_dir_all(&src).unwrap();
        let _ = std::fs::create_dir_all(&dest);
        std::fs::write(
            src.join("Makefile"),
            "install:\n\tmkdir -p $(PREFIX)/bin\n\tcp hello $(PREFIX)/bin/hello\n",
        )
        .unwrap();
        std::fs::write(src.join("hello"), b"#!/bin/sh\necho hi\n").unwrap();
        let jobs = 1;
        let result =
            build(BuildSystem::Make, &src, &dest, jobs).expect("make build should succeed");
        assert_eq!(result.build_system, "make");
        assert!(dest.join("bin/hello").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn split_words_handles_quotes_and_escapes() {
        assert_eq!(split_words("a b c"), vec!["a", "b", "c"]);
        assert_eq!(
            split_words("make install PREFIX=\"/a b/c\""),
            vec!["make", "install", "PREFIX=/a b/c"]
        );
        assert_eq!(
            split_words("sh -c 'echo hi there'"),
            vec!["sh", "-c", "echo hi there"]
        );
        assert_eq!(split_words("a\\ b c"), vec!["a b", "c"]);
        assert_eq!(split_words("  "), Vec::<String>::new());
    }

    #[test]
    fn custom_steps_substitute_prefix() {
        let base = std::env::temp_dir().join(format!("maca-src-steps-{}", std::process::id()));
        let src = base.join("src");
        let dest = base.join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("hello"), b"x").unwrap();
        let steps = vec![
            "mkdir -p {{prefix}}/bin".to_string(),
            "cp hello {{prefix}}/bin/hello".to_string(),
        ];
        run_steps(&src, &dest, &steps, 1).expect("steps should succeed");
        assert!(dest.join("bin/hello").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
