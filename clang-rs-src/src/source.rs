//! Locating, downloading, verifying and unpacking the upstream sources.

use std::fs;
use std::io::{self, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use crate::util::{self, Result};

/// An upstream release tarball pinned by checksum.
pub(crate) struct Tarball {
    /// Name of the project (`LLVM`, `libffi`, `ncurses`).
    pub name: &'static str,
    pub version: &'static str,
    pub file: &'static str,
    pub sha256: &'static str,
    /// Download locations, tried in order.
    pub urls: &'static [&'static str],
    /// Name of the single top-level directory inside the archive.
    pub dir: &'static str,
}

impl Tarball {
    pub fn major(&self) -> u32 {
        self.version
            .split('.')
            .next()
            .and_then(|m| m.parse().ok())
            .unwrap_or(0)
    }
}

macro_rules! llvm_release {
    ($version:literal, $sha256:literal) => {
        Tarball {
            name: "LLVM",
            version: $version,
            file: concat!("llvm-project-", $version, ".src.tar.xz"),
            sha256: $sha256,
            urls: &[concat!(
                "https://github.com/llvm/llvm-project/releases/download/llvmorg-",
                $version,
                "/llvm-project-",
                $version,
                ".src.tar.xz"
            )],
            dir: concat!("llvm-project-", $version, ".src"),
        }
    };
}

/// The LLVM releases that can be built, oldest first: the last release of
/// each major version. Every entry needs a `clang_<major>_0` feature.
pub(crate) const LLVM_RELEASES: &[Tarball] = &[
    llvm_release!(
        "14.0.6",
        "8b3cfd7bc695bd6cea0f37f53f0981f34f87496e79e2529874fd03a2f9dd3a8a"
    ),
    llvm_release!(
        "15.0.7",
        "8b5fcb24b4128cf04df1b0b9410ce8b1a729cb3c544e6da885d234280dedeac6"
    ),
    llvm_release!(
        "16.0.6",
        "ce5e71081d17ce9e86d7cbcfa28c4b04b9300f8fb7e78422b1feb6bc52c3028e"
    ),
    llvm_release!(
        "17.0.6",
        "58a8818c60e6627064f312dbf46c02d9949956558340938b71cf731ad8bc0813"
    ),
    llvm_release!(
        "18.1.8",
        "0b58557a6d32ceee97c8d533a59b9212d87e0fc4d2833924eb6c611247db2f2a"
    ),
    llvm_release!(
        "19.1.7",
        "82401fea7b79d0078043f7598b835284d6650a75b93e64b6f761ea7b63097501"
    ),
    llvm_release!(
        "20.1.8",
        "6898f963c8e938981e6c4a302e83ec5beb4630147c7311183cf61069af16333d"
    ),
    llvm_release!(
        "21.1.8",
        "4633a23617fa31a3ea51242586ea7fb1da7140e426bd62fc164261fe036aa142"
    ),
    llvm_release!(
        "22.1.8",
        "922f1817a0df7b1489272d18134ee0087a8b068828f87ac63b9861b1a9965888"
    ),
    llvm_release!(
        "23.1.3",
        "c44186a7762ed28954be72e5ff6df9808e0779d4f1bf014ecc4e7e211d31ee34"
    ),
];

/// The pinned release of an LLVM major version.
pub(crate) fn llvm_release(major: u32) -> Option<&'static Tarball> {
    LLVM_RELEASES.iter().find(|t| t.major() == major)
}

pub(crate) const LIBFFI: Tarball = Tarball {
    name: "libffi",
    version: "3.8.0",
    file: "libffi-3.8.0.tar.gz",
    sha256: "7da3e2d9a171eb0a038f592ecad3ff2bb2550f3496d87b3b29ad0cf4430c0db4",
    urls: &["https://github.com/libffi/libffi/releases/download/v3.8.0/libffi-3.8.0.tar.gz"],
    dir: "libffi-3.8.0",
};

pub(crate) const NCURSES: Tarball = Tarball {
    name: "ncurses",
    version: "6.6",
    file: "ncurses-6.6.tar.gz",
    sha256: "355b4cbbed880b0381a04c46617b7656e362585d52e9cf84a67e2009b749ff11",
    urls: &[
        "https://invisible-island.net/archives/ncurses/ncurses-6.6.tar.gz",
        "https://ftpmirror.gnu.org/ncurses/ncurses-6.6.tar.gz",
    ],
    dir: "ncurses-6.6",
};

const STAMP: &str = ".clang-rs-src-unpacked";
/// Bumped whenever the set of unpacked files changes, so that trees unpacked
/// by an older version of this crate are refreshed.
const LAYOUT: u32 = 2;

/// Where archives are downloaded to and unpacked.
pub(crate) struct Fetcher<'a> {
    /// Directory the archives are unpacked into.
    pub root: &'a Path,
    /// Directory holding the downloaded archives. Archives already present
    /// (with the right checksum) are not downloaded again.
    pub downloads: &'a Path,
    /// Refuse to download anything.
    pub offline: bool,
}

impl Fetcher<'_> {
    /// Downloads (unless already present), verifies and unpacks a pinned
    /// tarball, returning the unpacked source tree.
    pub fn fetch(
        &self,
        tarball: &Tarball,
        url_override: Option<&str>,
        keep: impl Fn(&Path) -> bool,
    ) -> Result<PathBuf> {
        let dest = self.root.join(tarball.dir);
        if is_unpacked(&dest, tarball.sha256) {
            return Ok(dest);
        }
        let archive = self.archive(tarball, url_override)?;
        println!("unpacking {}", archive.display());
        unpack(&archive, tarball.dir, self.root, tarball.sha256, keep)?;
        Ok(dest)
    }

    /// Returns the verified archive of `tarball`, downloading it if needed.
    fn archive(&self, tarball: &Tarball, url_override: Option<&str>) -> Result<PathBuf> {
        let archive = self.downloads.join(tarball.file);
        if archive.is_file() {
            if util::sha256_file(&archive)? == tarball.sha256 {
                return Ok(archive);
            }
            println!(
                "{} has the wrong checksum, downloading it again",
                archive.display()
            );
            let _ = fs::remove_file(&archive);
        }
        if self.offline {
            return Err(format!(
                "{} {} is needed but downloads are disabled (CLANG_RS_SRC_OFFLINE): \
                 download {} into {}",
                tarball.name,
                tarball.version,
                tarball.urls[0],
                self.downloads.display()
            ));
        }
        let urls: Vec<&str> = match url_override {
            Some(url) => vec![url],
            None => tarball.urls.to_vec(),
        };
        download(&urls, &archive).map_err(|e| {
            format!(
                "{e}\nDownload it manually into {} (or set CLANG_RS_SRC_DOWNLOAD_DIR to a \
                 directory containing it).",
                self.downloads.display()
            )
        })?;
        if let Err(e) = verify(&archive, tarball.sha256) {
            let _ = fs::remove_file(&archive);
            return Err(e);
        }
        Ok(archive)
    }
}

/// Where the LLVM sources come from.
pub(crate) struct LlvmSourceOptions<'a> {
    /// The pinned release to download.
    pub release: &'static Tarball,
    /// An already unpacked `llvm-project` checkout.
    pub source_dir: Option<&'a Path>,
    /// A local `llvm-project-*.src.tar.xz`.
    pub tarball: Option<&'a Path>,
    /// Overrides the download URL of the pinned tarball (e.g. a mirror).
    pub url: Option<&'a str>,
}

/// An unpacked `llvm-project` source tree.
pub(crate) struct LlvmSource {
    pub root: PathBuf,
    /// Identifies the contents: the SHA-256 of the tarball it came from, or
    /// the path of a user-provided tree.
    pub id: String,
}

/// Returns the root of an `llvm-project` source tree, downloading and
/// unpacking the pinned release if necessary.
pub(crate) fn llvm_source(
    fetcher: &Fetcher<'_>,
    opts: &LlvmSourceOptions<'_>,
) -> Result<LlvmSource> {
    if let Some(dir) = opts.source_dir {
        check_llvm_tree(dir)?;
        return Ok(LlvmSource {
            root: dir.to_path_buf(),
            id: format!("dir:{}", dir.display()),
        });
    }

    let Some(path) = opts.tarball else {
        let root = fetcher.fetch(opts.release, opts.url, keep_llvm_entry)?;
        check_llvm_tree(&root)?;
        return Ok(LlvmSource {
            root,
            id: opts.release.sha256.to_string(),
        });
    };

    // A user-supplied tarball is not pinned; derive the top-level directory
    // name from the file name and trust its contents.
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("invalid LLVM tarball path {}", path.display()))?;
    let top_dir = name
        .strip_suffix(".tar.xz")
        .ok_or_else(|| format!("expected a .tar.xz LLVM tarball, got {name}"))?;
    let sha256 = util::sha256_file(path)?;
    let dest = fetcher.root.join(top_dir);
    if !is_unpacked(&dest, &sha256) {
        println!("unpacking {}", path.display());
        unpack(path, top_dir, fetcher.root, &sha256, keep_llvm_entry)?;
    }
    check_llvm_tree(&dest)?;
    Ok(LlvmSource {
        root: dest,
        id: sha256,
    })
}

/// Only the parts of `llvm-project` that are needed to build libclang are
/// unpacked; the test suites alone are well over a gigabyte.
fn keep_llvm_entry(rel: &Path) -> bool {
    let parts: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [] => true,
        ["llvm" | "clang", "test" | "unittests" | "docs" | "www" | "benchmarks", ..] => false,
        // LLVM's Support library uses header-only parts of LLVM's libc.
        ["libc", "test" | "benchmarks" | "docs" | "fuzzing", ..] => false,
        [first, ..] => matches!(*first, "llvm" | "clang" | "cmake" | "third-party" | "libc"),
    }
}

fn check_llvm_tree(dir: &Path) -> Result<()> {
    for sub in [
        "llvm/CMakeLists.txt",
        "clang/CMakeLists.txt",
        "cmake/Modules",
    ] {
        if !dir.join(sub).exists() {
            return Err(format!(
                "{} does not look like an llvm-project source tree (missing {sub})",
                dir.display()
            ));
        }
    }
    Ok(())
}

/// The `major.minor.patch` version of an `llvm-project` source tree.
pub(crate) fn llvm_tree_version(root: &Path) -> Result<(u32, u32, u32)> {
    let candidates = [
        root.join("cmake/Modules/LLVMVersion.cmake"),
        root.join("llvm/CMakeLists.txt"),
    ];
    for file in &candidates {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let get = |name: &str| -> Option<u32> {
            let pat = format!("set({name} ");
            let start = text.find(&pat)? + pat.len();
            let rest = &text[start..];
            rest[..rest.find(')')?].trim().parse().ok()
        };
        if let (Some(major), Some(minor), Some(patch)) = (
            get("LLVM_VERSION_MAJOR"),
            get("LLVM_VERSION_MINOR"),
            get("LLVM_VERSION_PATCH"),
        ) {
            return Ok((major, minor, patch));
        }
    }
    Err(format!(
        "could not determine the LLVM version of {}",
        root.display()
    ))
}

fn is_unpacked(dest: &Path, sha256: &str) -> bool {
    fs::read_to_string(dest.join(STAMP)).is_ok_and(|s| s == stamp(sha256))
}

fn stamp(sha256: &str) -> String {
    format!("{sha256} layout={LAYOUT}\n")
}

fn verify(archive: &Path, expected: &str) -> Result<()> {
    let actual = util::sha256_file(archive)?;
    if actual != expected {
        return Err(format!(
            "checksum mismatch for {}: expected sha256 {expected}, got {actual}",
            archive.display()
        ));
    }
    Ok(())
}

/// Unpacks `top_dir` from `archive` into `dest_root/top_dir`. The tree is
/// unpacked into a temporary directory first and then renamed into place so
/// that concurrent builds sharing a cache never observe a partial tree.
fn unpack(
    archive: &Path,
    top_dir: &str,
    dest_root: &Path,
    sha256: &str,
    keep: impl Fn(&Path) -> bool,
) -> Result<()> {
    let dest = dest_root.join(top_dir);
    let tmp_root = dest_root.join(format!(".tmp-{top_dir}-{}", util::unique_suffix()));
    util::create_dir_all(&tmp_root)?;

    let file = fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let reader = BufReader::with_capacity(1 << 20, file);
    let name = archive.to_string_lossy();
    let decoder: Box<dyn Read> = if name.ends_with(".xz") {
        Box::new(liblzma::read::XzDecoder::new(reader))
    } else if name.ends_with(".gz") || name.ends_with(".tgz") {
        Box::new(flate2::read::GzDecoder::new(reader))
    } else {
        return Err(format!("unsupported archive format: {name}"));
    };

    let mut tar = tar::Archive::new(decoder);
    tar.set_preserve_mtime(true);
    let err = |e: std::io::Error| format!("failed to unpack {}: {e}", archive.display());
    for entry in tar.entries().map_err(err)? {
        let mut entry = entry.map_err(err)?;
        let path = entry.path().map_err(err)?.into_owned();
        let Ok(rel) = path.strip_prefix(top_dir) else {
            continue;
        };
        if !keep(rel) {
            continue;
        }
        // Creating symlinks needs special privileges on Windows; none of the
        // links in the source trees are needed to build.
        if cfg!(windows) && entry.header().entry_type().is_symlink() {
            continue;
        }
        entry.unpack_in(&tmp_root).map_err(err)?;
    }

    let unpacked = tmp_root.join(top_dir);
    util::write(&unpacked.join(STAMP), stamp(sha256))?;
    util::remove_dir_all(&dest)?;
    util::publish_dir(&unpacked, &dest)?;
    util::remove_dir_all(&tmp_root)
}

/// Downloads the first of `urls` that works to `dest`. Transient failures
/// (network problems, server errors) are retried a few times.
fn download(urls: &[&str], dest: &Path) -> Result<()> {
    util::create_dir_all(dest.parent().unwrap())?;
    let tmp = dest.with_file_name(format!(
        "{}.part-{}",
        dest.file_name().unwrap().to_string_lossy(),
        util::unique_suffix()
    ));

    let mut errors = Vec::new();
    for url in urls {
        for attempt in 1..=3u64 {
            if attempt > 1 {
                thread::sleep(Duration::from_secs(5 * (attempt - 1)));
            }
            println!("downloading {url}");
            match fetch(url, &tmp) {
                Ok(()) => {
                    let bytes = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
                    println!("downloaded {:.1} MB", bytes as f64 / 1e6);
                    return fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display()));
                }
                Err(failure) => {
                    let _ = fs::remove_file(&tmp);
                    errors.push(format!("{url}: {}", failure.message));
                    if !failure.retry {
                        break;
                    }
                }
            }
        }
    }
    Err(format!(
        "failed to download {}:\n  {}",
        dest.file_name().unwrap().to_string_lossy(),
        errors.join("\n  ")
    ))
}

struct Failure {
    message: String,
    retry: bool,
}

/// Downloads `url` to the file at `path` with the system's `curl` (included
/// with Windows 10+ and macOS), or else `wget` (PowerShell on Windows).
///
/// The system tools are used instead of an HTTP client crate because a build
/// script's dependencies are compiled for the host, which breaks (e.g. for
/// the C code in TLS libraries) when cross-building with the target's
/// toolchain, e.g. `CARGO_BUILD_TARGET=x86_64-unknown-linux-musl` on a glibc
/// host. They also use the operating system's trust store and proxy settings.
fn fetch(url: &str, path: &Path) -> std::result::Result<(), Failure> {
    let user_agent = concat!("clang-rs-src/", env!("CARGO_PKG_VERSION"));
    let mut curl = Command::new("curl");
    if cfg!(windows) {
        // curl.exe (shipped with Windows 10+) uses Schannel, which fails when
        // it cannot reach the certificate revocation servers, as is common
        // behind corporate proxies. The checksum is verified anyway.
        curl.arg("--ssl-no-revoke");
    }
    curl.args(["--location", "--silent", "--show-error", "--fail"])
        .args(["--connect-timeout", "30", "--user-agent", user_agent])
        // Print the status code to tell client errors from transient ones.
        .args(["--write-out", "%{http_code}"])
        .arg("--output")
        .arg(path)
        .arg(url);
    match run_quiet(&mut curl) {
        Ok(out) if out.status.success() => return Ok(()),
        Ok(out) => {
            let code = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let http = code.trim().parse::<u32>().unwrap_or(0);
            // curl's exit codes 3 (malformed URL) and 22 (HTTP error, with
            // --fail) are permanent unless the server failed or is busy.
            let permanent = match out.status.code() {
                Some(3) => true,
                Some(22) => (400..500).contains(&http) && http != 429,
                _ => false,
            };
            let reason = match stderr.trim() {
                "" => format!("curl exited with {}", out.status),
                s => s.to_string(),
            };
            return Err(Failure {
                message: reason,
                retry: !permanent,
            });
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Failure {
                message: format!("failed to run curl: {e}"),
                retry: false,
            })
        }
    }

    let (tool, mut fallback) = if cfg!(windows) {
        // Windows before 10 version 1803 has no curl.exe. Pass the URL and
        // path through the environment to avoid any quoting problems.
        let mut ps = Command::new("powershell");
        ps.args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(
                "$ProgressPreference = 'SilentlyContinue'; \
                 [Net.ServicePointManager]::SecurityProtocol = 'Tls12'; \
                 Invoke-WebRequest -UseBasicParsing -UserAgent $env:CLANG_RS_SRC_UA \
                 -Uri $env:CLANG_RS_SRC_URL -OutFile $env:CLANG_RS_SRC_OUT",
            )
            .env("CLANG_RS_SRC_UA", user_agent)
            .env("CLANG_RS_SRC_URL", url)
            .env("CLANG_RS_SRC_OUT", path);
        ("powershell", ps)
    } else {
        let mut wget = Command::new("wget");
        wget.args([
            "--quiet",
            "--tries=1",
            "--timeout=30",
            "--user-agent",
            user_agent,
        ])
        .arg("--output-document")
        .arg(path)
        .arg(url);
        ("wget", wget)
    };
    match run_quiet(&mut fallback) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(Failure {
            message: match String::from_utf8_lossy(&out.stderr).trim() {
                "" => format!("{tool} exited with {}", out.status),
                s => s.to_string(),
            },
            // wget exits with 8 for server-issued error responses (4xx/5xx).
            retry: !(tool == "wget" && out.status.code() == Some(8)),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Failure {
            message: format!(
                "neither `curl` nor `{tool}` was found on PATH; install one of them, or \
                 download the tarball manually into CLANG_RS_SRC_DOWNLOAD_DIR"
            ),
            retry: false,
        }),
        Err(e) => Err(Failure {
            message: format!("failed to run {tool}: {e}"),
            retry: false,
        }),
    }
}

fn run_quiet(cmd: &mut Command) -> io::Result<Output> {
    cmd.stdin(Stdio::null()).output()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llvm_filter() {
        let keep = |p: &str| keep_llvm_entry(Path::new(p));
        assert!(keep(""));
        assert!(keep("llvm/CMakeLists.txt"));
        assert!(keep("llvm/lib/Support/APInt.cpp"));
        assert!(keep("clang/tools/libclang/CIndex.cpp"));
        assert!(keep("cmake/Modules/LLVMVersion.cmake"));
        assert!(keep("third-party/siphash/include/siphash/SipHash.h"));
        assert!(keep("libc/shared/math.h"));
        assert!(keep("libc/src/__support/CPP/bit.h"));
        assert!(!keep("libc/test/src/math/sin_test.cpp"));
        assert!(!keep("llvm/test/CodeGen/X86/add.ll"));
        assert!(!keep("clang/test/Sema/attr.c"));
        assert!(!keep("clang/docs/index.rst"));
        assert!(!keep("lldb/CMakeLists.txt"));
        assert!(!keep("compiler-rt/lib/asan/asan.cpp"));
    }

    #[test]
    fn llvm_releases() {
        let mut previous = 0;
        for t in LLVM_RELEASES {
            assert!(t.major() > previous, "releases must be sorted and unique");
            previous = t.major();
            assert_eq!(t.file, format!("{}.tar.xz", t.dir));
            assert!(t.urls[0].ends_with(t.file));
            assert_eq!(t.sha256.len(), 64);
            assert_eq!(llvm_release(t.major()).unwrap().version, t.version);
        }
        assert!(llvm_release(previous + 1).is_none());
    }

    /// Downloads and verifies the dependency tarballs and the newest LLVM
    /// release, and checks that every other pinned URL exists. Needs network
    /// access: `cargo test -p clang-rs-src -- --ignored`.
    #[test]
    #[ignore]
    fn download_pinned_tarballs() {
        let dir =
            std::env::temp_dir().join(format!("clang-rs-src-download-{}", util::unique_suffix()));
        let fetcher = Fetcher {
            root: &dir,
            downloads: &dir,
            offline: false,
        };
        for tarball in [&LIBFFI, &NCURSES, LLVM_RELEASES.last().unwrap()] {
            let archive = fetcher.archive(tarball, None).unwrap();
            assert_eq!(util::sha256_file(&archive).unwrap(), tarball.sha256);
        }
        util::remove_dir_all(&dir).unwrap();

        for tarball in LLVM_RELEASES.iter().chain([&LIBFFI, &NCURSES]) {
            for url in tarball.urls {
                let mut curl = Command::new("curl");
                curl.args(["--location", "--silent", "--show-error", "--fail", "--head"])
                    .arg(url);
                let out = run_quiet(&mut curl).unwrap();
                assert!(
                    out.status.success(),
                    "{url}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }

    #[test]
    fn dependency_tarballs() {
        for t in [&LIBFFI, &NCURSES] {
            assert!(t.file.starts_with(t.dir));
            assert!(t.urls.iter().all(|url| url.ends_with(t.file)));
            assert_eq!(t.sha256.len(), 64);
        }
    }
}
