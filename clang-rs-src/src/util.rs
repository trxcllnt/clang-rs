use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub(crate) type Result<T, E = String> = std::result::Result<T, E>;

/// Runs a command, inheriting stdout/stderr so the output ends up in Cargo's
/// build script log.
pub(crate) fn run(cmd: &mut Command, desc: &str) -> Result<()> {
    println!("running ({desc}): {cmd:?}");
    match cmd.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "{desc} failed: `{}` exited with {status}\ncommand: {cmd:?}",
            program_name(cmd)
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!(
            "{desc} failed: could not find `{}` (is it installed and on PATH?)\ncommand: {cmd:?}",
            program_name(cmd)
        )),
        Err(e) => Err(format!("{desc} failed: {e}\ncommand: {cmd:?}")),
    }
}

/// Runs a command and returns its trimmed stdout.
pub(crate) fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run {cmd:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{cmd:?} exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn program_name(cmd: &Command) -> String {
    cmd.get_program().to_string_lossy().into_owned()
}

/// Returns whether `program --version` (or another probe argument) runs.
pub(crate) fn probe(program: &OsStr, arg: &str) -> bool {
    Command::new(program)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Finds an executable on `PATH`.
pub(crate) fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_string())
            .split(';')
            .map(|s| s.to_ascii_lowercase())
            .chain(std::iter::once(String::new()))
            .collect()
    } else {
        vec![String::new()]
    };
    env::split_paths(&env::var_os("PATH")?)
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .find(|candidate| candidate.is_file())
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

pub(crate) fn sha256_str(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A string unique to this process and moment, for temporary paths.
pub(crate) fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

pub(crate) fn create_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| format!("failed to create {}: {e}", path.display()))
}

pub(crate) fn remove_dir_all(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("failed to remove {}: {e}", path.display())),
    }
}

pub(crate) fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    fs::write(path, contents).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

pub(crate) fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))
}

/// Hard-links `src` to `dst` (falling back to a copy), replacing `dst`.
pub(crate) fn link_or_copy(src: &Path, dst: &Path) -> Result<()> {
    let _ = fs::remove_file(dst);
    if fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    fs::copy(src, dst)
        .map(drop)
        .map_err(|e| format!("failed to copy {} to {}: {e}", src.display(), dst.display()))
}

/// Recursively copies a directory tree (files are hard-linked when possible).
pub(crate) fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    create_dir_all(dst)?;
    let entries = fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", src.display()))?;
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            link_or_copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Moves a freshly produced directory into place. If another process won the
/// race and `dst` already exists, our copy is discarded.
pub(crate) fn publish_dir(tmp: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        create_dir_all(parent)?;
    }
    match fs::rename(tmp, dst) {
        Ok(()) => Ok(()),
        Err(_) if dst.exists() => remove_dir_all(tmp),
        Err(e) => Err(format!(
            "failed to move {} to {}: {e}",
            tmp.display(),
            dst.display()
        )),
    }
}

/// Formats a path the way CMake expects on the command line (forward slashes).
pub(crate) fn cmake_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// Reads an environment variable, preferring target-specific spellings the
/// same way the `cc` and `cmake` crates do.
pub(crate) fn target_env(var: &str, target: &str, host: &str) -> Option<String> {
    let kind = if target == host { "HOST" } else { "TARGET" };
    let target_u = target.replace('-', "_");
    [
        format!("{var}_{target}"),
        format!("{var}_{target_u}"),
        format!("{kind}_{var}"),
        var.to_string(),
    ]
    .iter()
    .find_map(|name| env::var(name).ok())
}

/// Number of parallel jobs to use for native builds.
pub(crate) fn num_jobs() -> usize {
    env::var("NUM_JOBS")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1)
}
