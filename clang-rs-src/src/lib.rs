//! Builds [libclang] — together with every Clang and LLVM library it depends
//! on — from source as **static libraries**, along with [libffi] and
//! [ncurses], for the target Cargo is compiling for.
//!
//! This crate is modeled after [`openssl-src`]: it is meant to be used as a
//! build dependency of a `-sys` crate, usually behind a `vendored` feature.
//!
//! ```no_run
//! // build.rs of a -sys crate
//! let artifacts = clang_rs_src::Build::new().build();
//! artifacts.print_cargo_metadata();
//! ```
//!
//! # Choosing the LLVM release
//!
//! The `clang_<major>_0` features select the LLVM/Clang release that is
//! built: the newest release of that major version (see
//! [`supported_llvm_versions`]). As in `clang-sys`, each feature implies the
//! ones for older versions, and the highest enabled feature wins. Without any
//! of them the newest supported release is built. [`Build::llvm_version`]
//! overrides the features.
//!
//! # What gets built
//!
//! 1. **libffi** and **ncurses** are built from their release tarballs (with
//!    autotools; libffi is compiled directly with `cl.exe` for MSVC targets,
//!    where ncurses is not applicable).
//! 2. When cross compiling, the LLVM/Clang TableGen tools are built for the
//!    host.
//! 3. LLVM and Clang are configured with CMake for the target and the static
//!    `libclang` library is built. LLVM is pointed at the libffi built in step 1
//!    (`LLVM_ENABLE_FFI`) — and, for LLVM releases that still support terminfo,
//!    at the ncurses from step 1 — while every other optional dependency (zlib,
//!    zstd, libxml2, libedit, ...) is disabled, so nothing is picked up from
//!    system library paths.
//! 4. The exact set of static libraries libclang needs is computed from the
//!    CMake exports and installed, together with the headers and Clang's
//!    builtin headers ("resource directory"), into one directory.
//!
//! No sources are bundled with this crate: the release tarballs of LLVM,
//! libffi and ncurses are downloaded at build time over HTTPS (with the
//! system's `curl` (or `wget`, or PowerShell on Windows), which check certificates
//! against the operating system's trust store and honor the system's proxy
//! settings, e.g. `HTTPS_PROXY`) and verified against pinned SHA-256 checksums. Tarballs that are already
//! in the download directory are not downloaded again, which also allows
//! offline builds.
//!
//! # Environment variables
//!
//! | Variable | Effect |
//! |---|---|
//! | `CLANG_RS_SRC_DOWNLOAD_DIR` | Where tarballs are downloaded to, and looked for first (default: `src/downloads` in the cache or build directory). |
//! | `CLANG_RS_SRC_OFFLINE` | Set to `1` to fail instead of downloading. |
//! | `CLANG_RS_SRC_LLVM_URL`, `CLANG_RS_SRC_LIBFFI_URL`, `CLANG_RS_SRC_NCURSES_URL` | Download the pinned tarball from this URL instead (e.g. a mirror); the checksum is still verified. |
//! | `CLANG_RS_SRC_LLVM_SOURCE_DIR` | Use an unpacked `llvm-project` tree instead of a pinned release. |
//! | `CLANG_RS_SRC_LLVM_TARBALL` | Use a local `llvm-project-*.src.tar.xz` instead of a pinned release. |
//! | `CLANG_RS_SRC_LLVM_TARGETS` | `LLVM_TARGETS_TO_BUILD` (default: the target's architecture; `all` for every backend). |
//! | `CLANG_RS_SRC_CACHE_DIR` | Share downloads, sources and finished builds between profiles and projects. |
//! | `CLANG_RS_SRC_BUILD_DIR` | Unpack and build here instead of `OUT_DIR` (`%TEMP%\clang-rs-src` on Windows, to keep paths short). |
//! | `CLANG_RS_SRC_CMAKE_ARGS` | Extra `-DKEY=VALUE` arguments (whitespace separated) for the LLVM build. |
//! | `CLANG_RS_SRC_STATIC_CXX_STDLIB` | Set to `1` to link libstdc++ statically on glibc targets (always done for musl). |
//!
//! Compilers are found exactly like the [`cc`] crate does (`CC_<target>`,
//! `CXX_<target>`, `AR_<target>`, `CFLAGS_<target>`, ...), and
//! `CMAKE_TOOLCHAIN_FILE_<target>`, `CMAKE_GENERATOR` and `CMAKE` are honored
//! like the `cmake` crate does.
//!
//! [libclang]: https://clang.llvm.org/docs/LibClang.html
//! [libffi]: https://sourceware.org/libffi/
//! [ncurses]: https://invisible-island.net/ncurses/
//! [`openssl-src`]: https://crates.io/crates/openssl-src
//! [`cc`]: https://crates.io/crates/cc

#![warn(missing_docs)]

use std::env;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

mod autotools;
mod cmake;
mod exports;
mod msvc_ffi;
mod source;
mod target;
mod toolchain;
mod util;

use crate::autotools::InstalledLib;
use crate::cmake::CMake;
use crate::exports::External;
use crate::source::{Fetcher, LlvmSourceOptions, LIBFFI, LLVM_RELEASES, NCURSES};
use crate::target::TargetInfo;
use crate::toolchain::{CxxStdlib, Toolchain};
use crate::util::Result;

/// The version of this crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The LLVM/Clang release that is built by default: the one selected by the
/// `clang_<major>_0` features, or the newest supported release.
pub fn llvm_version() -> &'static str {
    default_llvm_release().version
}

/// The LLVM/Clang releases that can be built, oldest first: the newest
/// release of each supported major version.
pub fn supported_llvm_versions() -> impl Iterator<Item = &'static str> {
    LLVM_RELEASES.iter().map(|t| t.version)
}

/// The libffi release that is built.
pub fn libffi_version() -> &'static str {
    LIBFFI.version
}

/// The ncurses release that is built.
pub fn ncurses_version() -> &'static str {
    NCURSES.version
}

/// The LLVM major version selected by the `clang_<major>_0` features. They
/// are cumulative, so the highest enabled one counts.
fn feature_llvm_major() -> Option<u32> {
    [
        (23, cfg!(feature = "clang_23_0")),
        (22, cfg!(feature = "clang_22_0")),
        (21, cfg!(feature = "clang_21_0")),
        (20, cfg!(feature = "clang_20_0")),
        (19, cfg!(feature = "clang_19_0")),
        (18, cfg!(feature = "clang_18_0")),
        (17, cfg!(feature = "clang_17_0")),
        (16, cfg!(feature = "clang_16_0")),
        (15, cfg!(feature = "clang_15_0")),
        (14, cfg!(feature = "clang_14_0")),
    ]
    .into_iter()
    .find_map(|(major, enabled)| enabled.then_some(major))
}

fn default_llvm_release() -> &'static source::Tarball {
    feature_llvm_major()
        .and_then(source::llvm_release)
        .unwrap_or_else(|| LLVM_RELEASES.last().unwrap())
}

/// Environment variables read by [`Build`] (in addition to the ones read by
/// the `cc` crate). A build script is rerun when any of them change.
pub const ENV_VARS: &[&str] = &[
    "CLANG_RS_SRC_DOWNLOAD_DIR",
    "CLANG_RS_SRC_OFFLINE",
    "CLANG_RS_SRC_LLVM_URL",
    "CLANG_RS_SRC_LIBFFI_URL",
    "CLANG_RS_SRC_NCURSES_URL",
    "CLANG_RS_SRC_LLVM_SOURCE_DIR",
    "CLANG_RS_SRC_LLVM_TARBALL",
    "CLANG_RS_SRC_LLVM_TARGETS",
    "CLANG_RS_SRC_CACHE_DIR",
    "CLANG_RS_SRC_BUILD_DIR",
    "CLANG_RS_SRC_CMAKE_ARGS",
    "CLANG_RS_SRC_STATIC_CXX_STDLIB",
    "CMAKE",
    "MAKE",
];

/// Variables that also exist in target-specific spellings
/// (`<VAR>_<target>`, `TARGET_<VAR>`, ...).
const TARGET_ENV_VARS: &[&str] = &["CMAKE_TOOLCHAIN_FILE", "CMAKE_GENERATOR", "CXXSTDLIB"];

const MANIFEST: &str = "clang-rs-src-manifest.txt";

/// Configures and runs the build of libclang and its dependencies.
#[derive(Clone, Debug)]
pub struct Build {
    out_dir: Option<PathBuf>,
    target: Option<String>,
    host: Option<String>,
    llvm_major: Option<u32>,
    llvm_targets: Option<String>,
    llvm_source_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    build_dir: Option<PathBuf>,
    static_crt: Option<bool>,
    cmake_defines: Vec<(String, String)>,
    jobs: Option<usize>,
}

/// The result of a [`Build`]: an installation directory containing the static
/// libraries, headers and Clang resource directory.
#[derive(Clone, Debug)]
pub struct Artifacts {
    root: PathBuf,
    include_dir: PathBuf,
    lib_dir: PathBuf,
    resource_dir: PathBuf,
    libs: Vec<String>,
    system_libs: Vec<String>,
    frameworks: Vec<String>,
    target: String,
    llvm_version: String,
}

impl Default for Build {
    fn default() -> Build {
        Build::new()
    }
}

fn env_nonempty(var: &str) -> Option<String> {
    env::var(var).ok().filter(|v| !v.is_empty())
}

impl Build {
    /// Creates a build configured from the Cargo build script environment
    /// (`TARGET`, `HOST`, `OUT_DIR`) and the `CLANG_RS_SRC_*` variables.
    pub fn new() -> Build {
        let cmake_defines = env_nonempty("CLANG_RS_SRC_CMAKE_ARGS")
            .map(|args| {
                args.split_whitespace()
                    .filter_map(|arg| {
                        let arg = arg.strip_prefix("-D").unwrap_or(arg);
                        let (k, v) = arg.split_once('=')?;
                        // Allow `-DKEY:TYPE=VALUE`.
                        let k = k.split(':').next().unwrap_or(k);
                        Some((k.to_string(), v.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Build {
            out_dir: env::var_os("OUT_DIR").map(|d| PathBuf::from(d).join("clang-rs-src")),
            target: env::var("TARGET").ok(),
            host: env::var("HOST").ok(),
            llvm_major: None,
            llvm_targets: env_nonempty("CLANG_RS_SRC_LLVM_TARGETS"),
            llvm_source_dir: env_nonempty("CLANG_RS_SRC_LLVM_SOURCE_DIR").map(PathBuf::from),
            cache_dir: env_nonempty("CLANG_RS_SRC_CACHE_DIR").map(PathBuf::from),
            build_dir: env_nonempty("CLANG_RS_SRC_BUILD_DIR").map(PathBuf::from),
            static_crt: None,
            cmake_defines,
            jobs: None,
        }
    }

    /// Directory for sources, build trees and the installation
    /// (default: `$OUT_DIR/clang-rs-src`).
    pub fn out_dir<P: AsRef<Path>>(&mut self, path: P) -> &mut Build {
        self.out_dir = Some(path.as_ref().to_path_buf());
        self
    }

    /// The Rust target triple to build for (default: `$TARGET`).
    pub fn target(&mut self, target: &str) -> &mut Build {
        self.target = Some(target.to_string());
        self
    }

    /// The Rust triple of the build machine (default: `$HOST`).
    pub fn host(&mut self, host: &str) -> &mut Build {
        self.host = Some(host.to_string());
        self
    }

    /// Builds the newest supported release of this LLVM major version (see
    /// [`supported_llvm_versions`]) instead of the one selected by the
    /// `clang_<major>_0` features.
    pub fn llvm_version(&mut self, major: u32) -> &mut Build {
        self.llvm_major = Some(major);
        self
    }

    /// The LLVM backends to build (`LLVM_TARGETS_TO_BUILD`), e.g. `"all"` or
    /// `"X86;AArch64"`. Defaults to the backend of the target architecture,
    /// or every backend with the `all-targets` feature.
    ///
    /// Clang can parse code for any target regardless of this setting; the
    /// backends are only needed for code generation and for validating inline
    /// assembly.
    pub fn llvm_targets(&mut self, targets: &str) -> &mut Build {
        self.llvm_targets = Some(targets.to_string());
        self
    }

    /// Builds the given unpacked `llvm-project` tree instead of the pinned
    /// release.
    pub fn llvm_source_dir<P: AsRef<Path>>(&mut self, path: P) -> &mut Build {
        self.llvm_source_dir = Some(path.as_ref().to_path_buf());
        self
    }

    /// A directory shared between builds: downloads and unpacked sources are
    /// kept there, and finished builds are stored there and reused by any
    /// later build with an identical configuration.
    pub fn cache_dir<P: AsRef<Path>>(&mut self, path: P) -> &mut Build {
        self.cache_dir = Some(path.as_ref().to_path_buf());
        self
    }

    /// Build in a subdirectory of this directory instead of [`out_dir`].
    /// Must not be used by concurrent builds of the same configuration.
    ///
    /// [`out_dir`]: Build::out_dir
    pub fn build_dir<P: AsRef<Path>>(&mut self, path: P) -> &mut Build {
        self.build_dir = Some(path.as_ref().to_path_buf());
        self
    }

    /// Link the static MSVC runtime (`/MT`). Defaults to whether the
    /// `crt-static` target feature is enabled.
    pub fn static_crt(&mut self, static_crt: bool) -> &mut Build {
        self.static_crt = Some(static_crt);
        self
    }

    /// Passes `-D<key>=<value>` to the CMake configuration of LLVM, overriding
    /// the defaults chosen by this crate.
    pub fn cmake_define(&mut self, key: &str, value: &str) -> &mut Build {
        self.cmake_defines
            .push((key.to_string(), value.to_string()));
        self
    }

    /// Number of parallel jobs (default: `$NUM_JOBS`).
    pub fn jobs(&mut self, jobs: usize) -> &mut Build {
        self.jobs = Some(jobs);
        self
    }

    /// Builds everything, exiting the process with an error message on
    /// failure. Use [`try_build`](Build::try_build) to handle errors.
    pub fn build(&mut self) -> Artifacts {
        match self.try_build() {
            Ok(artifacts) => artifacts,
            Err(e) => {
                println!("cargo:warning=clang-rs-src: failed to build libclang from source");
                eprintln!("\n\n\nclang-rs-src: {e}\n\n\n");
                std::process::exit(1);
            }
        }
    }

    /// Builds everything.
    pub fn try_build(&mut self) -> Result<Artifacts> {
        let target_triple = self.target.clone().ok_or("TARGET is not set")?;
        let host_triple = self.host.clone().ok_or("HOST is not set")?;
        let out_dir = self.out_dir.clone().ok_or("OUT_DIR is not set")?;
        let target = TargetInfo::parse(&target_triple)?;
        let host = TargetInfo::parse(&host_triple)?;
        let cross = target.triple != host.triple;
        let jobs = self.jobs.unwrap_or_else(util::num_jobs);

        for var in ENV_VARS {
            println!("cargo:rerun-if-env-changed={var}");
        }
        let target_u = target.triple.replace('-', "_");
        for var in TARGET_ENV_VARS {
            for name in [
                var.to_string(),
                format!("{var}_{}", target.triple),
                format!("{var}_{target_u}"),
                format!("TARGET_{var}"),
                format!("HOST_{var}"),
            ] {
                println!("cargo:rerun-if-env-changed={name}");
            }
        }

        let static_crt = target.is_msvc()
            && self.static_crt.unwrap_or_else(|| {
                env::var("CARGO_CFG_TARGET_FEATURE").is_ok_and(|f| f.contains("crt-static"))
            });

        // Where sources are unpacked and built. LLVM's deepest object files
        // are ~165 characters below its build directory, which exceeds
        // Windows' MAX_PATH when added to a typical Cargo OUT_DIR, so on
        // Windows the default is a short directory in the temp dir instead.
        // Only the installed artifacts need to live in OUT_DIR.
        let scratch_root = match &self.build_dir {
            Some(dir) => dir.clone(),
            None if cfg!(windows) => env::temp_dir().join("clang-rs-src"),
            None => out_dir.clone(),
        };

        // Sources.
        let src_root = match &self.cache_dir {
            Some(cache) => cache.join("src"),
            None => scratch_root.join("src"),
        };
        let downloads = env_nonempty("CLANG_RS_SRC_DOWNLOAD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| src_root.join("downloads"));
        let fetcher = Fetcher {
            root: &src_root,
            downloads: &downloads,
            offline: env::var("CLANG_RS_SRC_OFFLINE").is_ok_and(|v| v == "1"),
        };

        let requested_major = self.llvm_major.or_else(feature_llvm_major);
        let release = match requested_major {
            Some(major) => source::llvm_release(major).ok_or_else(|| {
                format!(
                    "LLVM {major} cannot be built; supported releases: {}",
                    supported_llvm_versions().collect::<Vec<_>>().join(", ")
                )
            })?,
            None => default_llvm_release(),
        };
        let llvm_tarball = env_nonempty("CLANG_RS_SRC_LLVM_TARBALL").map(PathBuf::from);
        let llvm_url = env_nonempty("CLANG_RS_SRC_LLVM_URL");
        let llvm_options = LlvmSourceOptions {
            release,
            source_dir: self.llvm_source_dir.as_deref(),
            tarball: llvm_tarball.as_deref(),
            url: llvm_url.as_deref(),
        };
        // The version and checksum of a pinned release are known up front, so
        // a cached build of it needs no sources at all. Other sources have to
        // be inspected.
        let (llvm_version, llvm_id, mut llvm_tree) =
            if llvm_options.source_dir.is_none() && llvm_options.tarball.is_none() {
                (
                    release.version.to_string(),
                    release.sha256.to_string(),
                    None,
                )
            } else {
                let llvm = source::llvm_source(&fetcher, &llvm_options)?;
                let (major, minor, patch) = source::llvm_tree_version(&llvm.root)?;
                (format!("{major}.{minor}.{patch}"), llvm.id, Some(llvm.root))
            };
        let major: u32 = llvm_version
            .split('.')
            .next()
            .and_then(|m| m.parse().ok())
            .unwrap_or(0);
        if let Some(requested) = requested_major {
            // A -sys crate exposes the API of the requested version, which
            // older sources would not provide.
            if major < requested {
                let by = match self.llvm_major {
                    // As clang-rs-sys does with the version whose API it
                    // declares.
                    Some(_) => "by the build script (`Build::llvm_version`)".to_string(),
                    None => format!("by the `clang_{requested}_0` feature"),
                };
                return Err(format!(
                    "the provided LLVM sources are version {llvm_version}, but LLVM \
                     {requested} or newer was requested {by}. Enable the \
                     `clang_{major}_0` feature to build these sources"
                ));
            }
        }

        // Toolchains.
        let tc = Toolchain::new(&target, &host, target.is_msvc().then_some(static_crt))?;
        let host_tc = if cross {
            Some(Toolchain::new(&host, &host, None)?)
        } else {
            None
        };
        let cxx_stdlib = tc.cxx_stdlib(&target, &host);
        let toolchain_file = util::target_env("CMAKE_TOOLCHAIN_FILE", &target.triple, &host.triple);
        let host_toolchain_file = [
            format!("CMAKE_TOOLCHAIN_FILE_{}", host.triple),
            format!("CMAKE_TOOLCHAIN_FILE_{}", host.triple.replace('-', "_")),
            "HOST_CMAKE_TOOLCHAIN_FILE".to_string(),
        ]
        .iter()
        .find_map(|v| env_nonempty(v));

        let llvm_targets = self.llvm_targets.clone().unwrap_or_else(|| {
            if cfg!(feature = "all-targets") {
                "all".to_string()
            } else {
                target.llvm_backend().unwrap_or("host").to_string()
            }
        });

        // Everything that influences the result, for caching.
        let mut config = String::new();
        let _ = writeln!(config, "clang-rs-src {}", version());
        let _ = writeln!(config, "llvm {llvm_version} {llvm_id}");
        let _ = writeln!(
            config,
            "libffi {} ncurses {}",
            LIBFFI.version, NCURSES.version
        );
        let _ = writeln!(config, "target {} host {}", target.triple, host.triple);
        let _ = writeln!(
            config,
            "llvm-targets {llvm_targets} static-crt {static_crt}"
        );
        let _ = writeln!(config, "cc {:?} {:?}", tc.c.path(), tc.c_flags);
        let _ = writeln!(config, "cxx {:?} {:?}", tc.cxx.path(), tc.cxx_flags);
        let _ = writeln!(config, "ar {:?} ranlib {:?}", tc.ar, tc.ranlib);
        let _ = writeln!(
            config,
            "toolchain-file {toolchain_file:?} {host_toolchain_file:?}"
        );
        let _ = writeln!(config, "cxx-stdlib {cxx_stdlib:?}");
        let _ = writeln!(config, "defines {:?}", self.cmake_defines);
        let key = util::sha256_str(&config)[..16].to_string();
        let config_name = format!("{}-{key}", target.triple);

        if let Some(cache) = &self.cache_dir {
            let cached = cache.join("artifacts").join(&config_name);
            if let Ok(artifacts) = Artifacts::load(&cached) {
                println!("using cached build {}", cached.display());
                return Ok(artifacts);
            }
        }

        let work = match &self.build_dir {
            // Shared by every build of this configuration (e.g. debug and
            // release), which is why concurrent builds must not use it.
            Some(dir) => dir.join(&config_name),
            // One directory per OUT_DIR, like the non-Windows default.
            None if cfg!(windows) => {
                scratch_root.join(&util::sha256_str(&out_dir.to_string_lossy())[..16])
            }
            None => out_dir.clone(),
        };
        util::create_dir_all(&work)?;
        let deps = work.join("deps");

        let llvm_src = match llvm_tree.take() {
            Some(tree) => tree,
            None => source::llvm_source(&fetcher, &llvm_options)?.root,
        };
        // LLVM dropped its terminfo dependency in release 19.
        let supports_terminfo = util::read_to_string(&llvm_src.join("llvm/CMakeLists.txt"))?
            .contains("LLVM_ENABLE_TERMINFO");
        let has_min_tblgen =
            util::read_to_string(&llvm_src.join("llvm/utils/TableGen/CMakeLists.txt"))
                .is_ok_and(|s| s.contains("llvm-min-tblgen"));

        // 1. libffi and ncurses.
        let ffi_src = fetcher.fetch(
            &LIBFFI,
            env_nonempty("CLANG_RS_SRC_LIBFFI_URL").as_deref(),
            |_| true,
        )?;
        let ffi = cached_step(&deps.join("libffi.stamp"), &config, || {
            if target.is_msvc() {
                msvc_ffi::build_libffi(&ffi_src, &deps, &target, &tc)
            } else {
                autotools::build_libffi(&ffi_src, &deps, &target, &host, &tc, jobs)
            }
        })?;
        let ncurses = if target.is_windows() {
            None
        } else {
            let src = fetcher.fetch(
                &NCURSES,
                env_nonempty("CLANG_RS_SRC_NCURSES_URL").as_deref(),
                |_| true,
            )?;
            Some(cached_step(&deps.join("ncurses.stamp"), &config, || {
                let build_tc = host_tc.as_ref().unwrap_or(&tc);
                autotools::build_ncurses(&src, &deps, &target, &host, &tc, build_tc, jobs)
            })?)
        };

        // 2. Host tools.
        let exe = if host.is_windows() { ".exe" } else { "" };
        let native_tools = match &host_tc {
            Some(host_tc) => {
                let mut cm = CMake {
                    src: llvm_src.join("llvm"),
                    build_dir: work.join("llvm-host-build"),
                    target: &host,
                    host: &host,
                    toolchain: host_tc,
                    cross: false,
                    toolchain_file: host_toolchain_file.clone(),
                    static_crt: false,
                    defines: Vec::new(),
                    env: Vec::new(),
                    env_remove: Vec::new(),
                    jobs,
                };
                common_defines(&mut cm);
                cm.define(
                    "LLVM_TARGETS_TO_BUILD",
                    host.llvm_backend().unwrap_or("host"),
                );
                cm.define("LLVM_ENABLE_FFI", "OFF");
                if supports_terminfo {
                    cm.define("LLVM_ENABLE_TERMINFO", "OFF");
                }
                cm.configure()?;
                let mut tools = vec!["llvm-tblgen", "clang-tblgen"];
                if has_min_tblgen {
                    tools.push("llvm-min-tblgen");
                }
                cm.build(&tools)?;
                Some(cm.bin_dir())
            }
            None => None,
        };

        // 3. LLVM and Clang.
        let mut cm = CMake {
            src: llvm_src.join("llvm"),
            build_dir: work.join("llvm-build"),
            target: &target,
            host: &host,
            toolchain: &tc,
            cross,
            toolchain_file: toolchain_file.clone(),
            static_crt,
            defines: Vec::new(),
            env: Vec::new(),
            env_remove: vec!["PKG_CONFIG_PATH", "PKG_CONFIG_SYSROOT_DIR"],
            jobs,
        };
        common_defines(&mut cm);
        cm.define("LLVM_TARGETS_TO_BUILD", llvm_targets.as_str());
        cm.define("LLVM_HOST_TRIPLE", target.llvm_triple());
        cm.define("LLVM_DEFAULT_TARGET_TRIPLE", target.llvm_triple());
        // Without PIC (only meaningful off Windows) libclang is built only as
        // a static library; with PIC both are configured and we build the
        // static one.
        cm.define(
            "LLVM_ENABLE_PIC",
            if target.is_windows() { "OFF" } else { "ON" },
        );
        cm.define("LIBCLANG_BUILD_STATIC", "ON");

        // Point LLVM at our libffi. Setting the result variables of FindFFI
        // directly keeps it from searching (and finding) a system libffi.
        let ffi_lib_dir = ffi.lib.parent().unwrap().to_path_buf();
        cm.define("LLVM_ENABLE_FFI", "FORCE_ON");
        cm.define_path("FFI_INCLUDE_DIR", &ffi.include_dir);
        cm.define_path("FFI_INCLUDE_DIRS", &ffi.include_dir);
        cm.define_path("FFI_LIBRARY_DIR", &ffi_lib_dir);
        cm.define_path("FFI_LIBRARIES", &ffi.lib);
        cm.define_path("FFI_STATIC_LIBRARIES", &ffi.lib);
        // pkg-config may only see our own .pc files.
        let pkgconfig_dir = ffi_lib_dir.join("pkgconfig");
        util::create_dir_all(&pkgconfig_dir)?;
        cm.env
            .push(("PKG_CONFIG_LIBDIR".into(), pkgconfig_dir.into_os_string()));

        if supports_terminfo {
            match &ncurses {
                Some(nc) => {
                    cm.define("LLVM_ENABLE_TERMINFO", "FORCE_ON");
                    cm.define_path("Terminfo_LIBRARIES", &nc.lib);
                }
                None => {
                    cm.define("LLVM_ENABLE_TERMINFO", "OFF");
                }
            }
        }
        if target.is_musl() {
            // Don't link a libexecinfo that may happen to be in the sysroot.
            cm.define("CMAKE_DISABLE_FIND_PACKAGE_Backtrace", "ON");
        }
        if let Some(tools) = &native_tools {
            cm.define_path("LLVM_NATIVE_TOOL_DIR", tools);
            cm.define_path("LLVM_TABLEGEN", &tools.join(format!("llvm-tblgen{exe}")));
            cm.define_path("CLANG_TABLEGEN", &tools.join(format!("clang-tblgen{exe}")));
            if has_min_tblgen {
                cm.define_path(
                    "LLVM_HEADERS_TABLEGEN",
                    &tools.join(format!("llvm-min-tblgen{exe}")),
                );
            }
        }
        for (k, v) in &self.cmake_defines {
            cm.define(k, v.as_str());
        }
        cm.configure()?;

        let export_files = [
            cm.build_dir.join("lib/cmake/llvm/LLVMExports.cmake"),
            cm.build_dir.join("lib/cmake/clang/ClangTargets.cmake"),
        ];
        let exported = exports::parse(&export_files)?;
        let root_target = if exported.contains_key("libclang_static") {
            "libclang_static"
        } else {
            "libclang"
        };
        cm.build(&[root_target])?;
        let closure = exports::closure(&exported, root_target)?;

        // 4. Install, straight into the cache if there is one.
        let install = match &self.cache_dir {
            Some(cache) => cache
                .join("artifacts")
                .join(format!(".tmp-{config_name}-{}", util::unique_suffix())),
            None => out_dir.join("install"),
        };
        util::remove_dir_all(&install)?;
        let lib_dir = install.join("lib");
        let include_dir = install.join("include");
        util::create_dir_all(&lib_dir)?;
        util::create_dir_all(&include_dir)?;

        let mut libs = Vec::new();
        let mut install_lib = |path: &Path| -> Result<()> {
            let file = path
                .file_name()
                .ok_or_else(|| format!("bad library path {}", path.display()))?;
            util::link_or_copy(path, &lib_dir.join(file))?;
            libs.push(link_name(path, target.is_msvc()));
            Ok(())
        };
        for (_, location) in &closure.libs {
            install_lib(location)?;
        }
        // libclang does not reference libffi or ncurses itself in current LLVM
        // releases, but they are linked so that every LLVM component that
        // does (and anything else in the final binary) resolves to these
        // copies rather than to a system library.
        install_lib(&ffi.lib)?;
        if let Some(nc) = &ncurses {
            install_lib(&nc.lib)?;
        }
        let mut system_libs = Vec::new();
        match &cxx_stdlib {
            CxxStdlib::None => {}
            CxxStdlib::Dylib(name) => system_libs.push(name.clone()),
            CxxStdlib::Static(path) => install_lib(path)?,
        }

        let mut frameworks = Vec::new();
        for external in &closure.externals {
            match external {
                External::Lib(name) => push_unique(&mut system_libs, name),
                External::Framework(name) => push_unique(&mut frameworks, name),
                External::Imported(name) => match name.as_str() {
                    "FFI::ffi" | "FFI::ffi_static" | "Terminfo::terminfo" => {}
                    "Threads::Threads" if !target.is_windows() && !target.is_apple() => {
                        push_unique(&mut system_libs, "pthread")
                    }
                    "Threads::Threads" => {}
                    other => {
                        println!(
                            "cargo:warning=clang-rs-src: ignoring unknown dependency `{other}`"
                        )
                    }
                },
                External::Path(path) if *path == ffi.lib => {}
                External::Path(path) if ncurses.as_ref().is_some_and(|nc| *path == nc.lib) => {}
                External::Path(path) => {
                    // Everything optional is disabled, so this would be a
                    // library from the build machine leaking into the build.
                    let is_archive = path
                        .extension()
                        .is_some_and(|ext| ext == "a" || ext == "lib");
                    if !is_archive {
                        return Err(format!(
                            "LLVM was configured to link {}, which is not part of this build; \
                             disable the LLVM feature that requires it",
                            path.display()
                        ));
                    }
                    println!(
                        "cargo:warning=clang-rs-src: LLVM links {}, which was not built by clang-rs-src",
                        path.display()
                    );
                    install_lib(path)?;
                }
            }
        }

        // Headers.
        util::copy_dir(
            &llvm_src.join("clang/include/clang-c"),
            &include_dir.join("clang-c"),
        )?;
        util::copy_dir(&ffi.include_dir, &include_dir)?;
        if let Some(nc) = &ncurses {
            util::copy_dir(&nc.include_dir, &include_dir)?;
        }

        // Clang's builtin headers, which libclang needs at runtime.
        let libclang_dir = closure.libs[0].1.parent().unwrap().to_path_buf();
        let resource_src = resource_dir_in(&libclang_dir, &llvm_version)?;
        // Keep the directory name: libclang looks for it by that name.
        let resource_rel = PathBuf::from("lib")
            .join("clang")
            .join(resource_src.file_name().unwrap());
        util::copy_dir(&resource_src, &install.join(&resource_rel))?;

        let artifacts = Artifacts {
            root: install.clone(),
            include_dir,
            lib_dir: install.join("lib"),
            resource_dir: install.join(&resource_rel),
            libs,
            system_libs,
            frameworks,
            target: target.triple.clone(),
            llvm_version,
        };
        artifacts.save(&resource_rel)?;

        match &self.cache_dir {
            Some(cache) => {
                let dst = cache.join("artifacts").join(&config_name);
                util::publish_dir(&install, &dst)?;
                Artifacts::load(&dst)
            }
            None => Ok(artifacts),
        }
    }
}

/// Defines shared by the host-tools and target builds of LLVM.
fn common_defines(cm: &mut CMake<'_>) {
    for (k, v) in [
        ("LLVM_ENABLE_PROJECTS", "clang"),
        ("LLVM_ENABLE_ASSERTIONS", "OFF"),
        ("LLVM_ENABLE_WARNINGS", "OFF"),
        ("LLVM_INCLUDE_TESTS", "OFF"),
        ("LLVM_INCLUDE_BENCHMARKS", "OFF"),
        ("LLVM_INCLUDE_EXAMPLES", "OFF"),
        ("LLVM_INCLUDE_DOCS", "OFF"),
        ("LLVM_INCLUDE_UTILS", "OFF"),
        ("LLVM_BUILD_TOOLS", "OFF"),
        ("LLVM_BUILD_UTILS", "OFF"),
        ("LLVM_ENABLE_BINDINGS", "OFF"),
        ("LLVM_ENABLE_OCAMLDOC", "OFF"),
        ("LLVM_BUILD_LLVM_DYLIB", "OFF"),
        ("LLVM_LINK_LLVM_DYLIB", "OFF"),
        ("BUILD_SHARED_LIBS", "OFF"),
        ("CLANG_LINK_CLANG_DYLIB", "OFF"),
        ("CLANG_INCLUDE_TESTS", "OFF"),
        ("CLANG_INCLUDE_DOCS", "OFF"),
        ("CLANG_BUILD_TOOLS", "OFF"),
        ("CLANG_BUILD_EXAMPLES", "OFF"),
        ("CLANG_PLUGIN_SUPPORT", "OFF"),
        // Optional dependencies are disabled so that nothing is picked up
        // from the build machine.
        ("LLVM_ENABLE_LIBXML2", "OFF"),
        ("CLANG_ENABLE_LIBXML2", "OFF"),
        ("LLVM_ENABLE_ZLIB", "OFF"),
        ("LLVM_ENABLE_ZSTD", "OFF"),
        ("LLVM_ENABLE_LIBEDIT", "OFF"),
        ("LLVM_ENABLE_LIBPFM", "OFF"),
        ("LLVM_ENABLE_CURL", "OFF"),
        ("LLVM_ENABLE_HTTPLIB", "OFF"),
        ("LLVM_ENABLE_Z3_SOLVER", "OFF"),
        ("LLVM_ENABLE_ICU", "OFF"),
        ("LLVM_ENABLE_ICONV", "OFF"),
        ("LLVM_ENABLE_DIA_SDK", "OFF"),
    ] {
        cm.define(k, v);
    }
    for package in [
        "ZLIB", "zstd", "LibXml2", "LibEdit", "Libpfm", "CURL", "httplib", "Z3", "ICU", "Iconv",
    ] {
        cm.define(&format!("CMAKE_DISABLE_FIND_PACKAGE_{package}"), "ON");
    }
}

/// Runs `step` unless a previous run with the same configuration finished.
fn cached_step(
    stamp: &Path,
    config: &str,
    step: impl FnOnce() -> Result<InstalledLib>,
) -> Result<InstalledLib> {
    let fingerprint = util::sha256_str(config);
    if let Ok(text) = std::fs::read_to_string(stamp) {
        let mut lines = text.lines();
        if lines.next() == Some(fingerprint.as_str()) {
            if let (Some(include), Some(lib)) = (lines.next(), lines.next()) {
                let lib = PathBuf::from(lib);
                if lib.is_file() {
                    return Ok(InstalledLib {
                        include_dir: PathBuf::from(include),
                        lib,
                    });
                }
            }
        }
    }
    let _ = std::fs::remove_file(stamp);
    let installed = step()?;
    util::write(
        stamp,
        format!(
            "{fingerprint}\n{}\n{}\n",
            installed.include_dir.display(),
            installed.lib.display()
        ),
    )?;
    Ok(installed)
}

/// Locates `clang/<major>` (the resource directory) next to the libraries.
/// Locates Clang's resource directory next to the libraries: `clang/<major>`
/// since LLVM 16, `clang/<major>.<minor>.<patch>` before.
fn resource_dir_in(lib_dir: &Path, llvm_version: &str) -> Result<PathBuf> {
    let major = llvm_version.split('.').next().unwrap_or(llvm_version);
    [major, llvm_version]
        .iter()
        .map(|name| lib_dir.join("clang").join(name))
        .find(|dir| dir.join("include").is_dir())
        .ok_or_else(|| {
            format!(
                "the Clang resource directory was not found in {}",
                lib_dir.join("clang").display()
            )
        })
}

/// The name to pass to `-l` for a library file.
fn link_name(path: &Path, msvc: bool) -> String {
    let stem = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default();
    let stem = stem
        .strip_suffix(".a")
        .or_else(|| stem.strip_suffix(".lib"))
        .unwrap_or(stem);
    if msvc {
        stem.to_string()
    } else {
        stem.strip_prefix("lib").unwrap_or(stem).to_string()
    }
}

fn push_unique(list: &mut Vec<String>, item: &str) {
    if !list.iter().any(|i| i == item) {
        list.push(item.to_string());
    }
}

impl Artifacts {
    /// The installation directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Contains `clang-c/*.h`, `ffi.h` and (except on Windows) the ncurses
    /// headers.
    pub fn include_dir(&self) -> &Path {
        &self.include_dir
    }

    /// Contains all static libraries listed by [`libs`](Artifacts::libs).
    pub fn lib_dir(&self) -> &Path {
        &self.lib_dir
    }

    /// Clang's resource directory (`lib/clang/<major>`), whose `include`
    /// subdirectory holds the compiler's builtin headers (`stddef.h`,
    /// `stdarg.h`, ...).
    ///
    /// Pass it as `-resource-dir=<path>` when parsing (programs that are
    /// distributed have to ship the headers), or system headers will fail to
    /// parse: libclang doesn't look next to itself. Without the option, it
    /// derives the directory from the compiler in `argv[0]`, as
    /// `<its directory>/../lib/clang/<major>`, which for
    /// `clang_parseTranslationUnit` (`argv[0]` is plain `clang`) is
    /// relative to the current directory.
    pub fn resource_dir(&self) -> &Path {
        &self.resource_dir
    }

    /// Static libraries to link, in dependency order (libclang first).
    pub fn libs(&self) -> &[String] {
        &self.libs
    }

    /// System libraries to link dynamically (e.g. `stdc++`, `pthread`).
    pub fn system_libs(&self) -> &[String] {
        &self.system_libs
    }

    /// Apple frameworks to link.
    pub fn frameworks(&self) -> &[String] {
        &self.frameworks
    }

    /// The Rust target triple the libraries were built for.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The version of LLVM/Clang that was built.
    pub fn llvm_version(&self) -> &str {
        &self.llvm_version
    }

    /// Prints the Cargo directives that link everything, and exports the
    /// installation paths as `DEP_<links>_*` metadata (`root`, `include`,
    /// `lib`, `resource_dir`, `llvm_version`) for dependent build scripts.
    pub fn print_cargo_metadata(&self) {
        // Rebuild if the installation disappears (e.g. a cleaned cache).
        println!(
            "cargo:rerun-if-changed={}",
            self.root.join(MANIFEST).display()
        );
        println!("cargo:rustc-link-search=native={}", self.lib_dir.display());
        for lib in &self.libs {
            println!("cargo:rustc-link-lib=static={lib}");
        }
        for lib in &self.system_libs {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
        for framework in &self.frameworks {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:root={}", self.root.display());
        println!("cargo:include={}", self.include_dir.display());
        println!("cargo:lib={}", self.lib_dir.display());
        println!("cargo:resource_dir={}", self.resource_dir.display());
        println!("cargo:llvm_version={}", self.llvm_version);
    }

    fn save(&self, resource_rel: &Path) -> Result<()> {
        let mut text = String::from("# Generated by clang-rs-src.\n");
        let _ = writeln!(text, "target={}", self.target);
        let _ = writeln!(text, "llvm_version={}", self.llvm_version);
        let _ = writeln!(
            text,
            "resource_dir={}",
            resource_rel.to_string_lossy().replace('\\', "/")
        );
        for lib in &self.libs {
            let _ = writeln!(text, "static={lib}");
        }
        for lib in &self.system_libs {
            let _ = writeln!(text, "dylib={lib}");
        }
        for framework in &self.frameworks {
            let _ = writeln!(text, "framework={framework}");
        }
        let path = self.root.join(MANIFEST);
        util::write(&path, text)?;
        // `print_cargo_metadata` makes the manifest a `rerun-if-changed`
        // input so that a deleted installation is rebuilt. Cargo considers
        // files written while a build script ran to be newer than that run,
        // so backdate the manifest; only its disappearance should count. A
        // zero timestamp is avoided on purpose: some platforms (Windows on
        // ARM) reject it, since a zero `FILETIME` means "leave unchanged".
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(86_400, 0))
            .map_err(|e| format!("failed to set the mtime of {}: {e}", path.display()))
    }

    /// Loads the artifacts of a previous build from its installation
    /// directory.
    fn load(root: &Path) -> Result<Artifacts> {
        let text = util::read_to_string(&root.join(MANIFEST))?;
        let mut artifacts = Artifacts {
            root: root.to_path_buf(),
            include_dir: root.join("include"),
            lib_dir: root.join("lib"),
            resource_dir: PathBuf::new(),
            libs: Vec::new(),
            system_libs: Vec::new(),
            frameworks: Vec::new(),
            target: String::new(),
            llvm_version: String::new(),
        };
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.to_string();
            match key {
                "target" => artifacts.target = value,
                "llvm_version" => artifacts.llvm_version = value,
                "resource_dir" => artifacts.resource_dir = root.join(value),
                "static" => artifacts.libs.push(value),
                "dylib" => artifacts.system_libs.push(value),
                "framework" => artifacts.frameworks.push(value),
                _ => {}
            }
        }
        if artifacts.target.is_empty() || artifacts.libs.is_empty() {
            return Err(format!("incomplete manifest in {}", root.display()));
        }
        Ok(artifacts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_names() {
        assert_eq!(link_name(Path::new("/x/libclang.a"), false), "clang");
        assert_eq!(
            link_name(Path::new("/x/libLLVMSupport.a"), false),
            "LLVMSupport"
        );
        assert_eq!(link_name(Path::new("/x/libstdc++.a"), false), "stdc++");
        assert_eq!(link_name(Path::new("C:/x/libclang.lib"), true), "libclang");
        assert_eq!(
            link_name(Path::new("C:/x/LLVMSupport.lib"), true),
            "LLVMSupport"
        );
        assert_eq!(link_name(Path::new("C:/x/ffi.lib"), true), "ffi");
    }

    /// Every supported release needs a `clang_<major>_0` feature, chained
    /// like clang-sys's, here and (in the workspace) in clang-rs-sys.
    #[test]
    fn version_features() {
        let majors: Vec<u32> = LLVM_RELEASES.iter().map(|t| t.major()).collect();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        let sys_manifest = std::fs::read_to_string(dir.join("../clang-rs-sys/Cargo.toml")).ok();
        let lib = include_str!("lib.rs");
        for (i, major) in majors.iter().enumerate() {
            let previous = (i > 0).then(|| format!("\"clang_{}_0\"", majors[i - 1]));
            let line = format!(
                "clang_{major}_0 = [{}]",
                previous.clone().unwrap_or_default()
            );
            assert!(manifest.contains(&line), "Cargo.toml lacks `{line}`");
            assert!(lib.contains(&format!("({major}, cfg!(feature = \"clang_{major}_0\"))")));
            if let Some(sys) = &sys_manifest {
                let forward = format!("\"clang-rs-src?/clang_{major}_0\"");
                let line = match &previous {
                    Some(previous) => format!("clang_{major}_0 = [{previous}, {forward}]"),
                    None => format!("clang_{major}_0 = [{forward}]"),
                };
                assert!(
                    sys.contains(&line),
                    "clang-rs-sys/Cargo.toml lacks `{line}`"
                );
            }
        }
        let declared = manifest
            .lines()
            .filter(|l| l.starts_with("clang_") && l.contains(" = ["))
            .count();
        assert_eq!(declared, majors.len(), "features for unsupported releases");
        // clang-rs-sys's build script and tests list them newest first, as
        // without a version feature the newest release is declared and built.
        for file in ["build/main.rs", "tests/parse.rs"] {
            let Ok(text) = std::fs::read_to_string(dir.join("../clang-rs-sys").join(file)) else {
                continue;
            };
            let positions: Vec<usize> = majors
                .iter()
                .rev()
                .map(|major| {
                    text.find(&format!("({major}, cfg!(feature = \"clang_{major}_0\"))"))
                        .unwrap_or_else(|| panic!("clang-rs-sys/{file} lacks clang_{major}_0"))
                })
                .collect();
            assert!(
                positions.windows(2).all(|w| w[0] < w[1]),
                "clang-rs-sys/{file} doesn't list the features newest first"
            );
        }
        if feature_llvm_major().is_none() {
            assert_eq!(default_llvm_release().major(), *majors.last().unwrap());
        }
    }

    #[test]
    fn manifest_round_trip() {
        let root = env::temp_dir().join(format!("clang-rs-src-test-{}", util::unique_suffix()));
        util::create_dir_all(&root).unwrap();
        let artifacts = Artifacts {
            root: root.clone(),
            include_dir: root.join("include"),
            lib_dir: root.join("lib"),
            resource_dir: root.join("lib/clang/23"),
            libs: vec!["clang".into(), "LLVMSupport".into(), "ffi".into()],
            system_libs: vec!["stdc++".into(), "m".into()],
            frameworks: vec![],
            target: "x86_64-unknown-linux-gnu".into(),
            llvm_version: "23.1.3".into(),
        };
        artifacts.save(Path::new("lib/clang/23")).unwrap();
        let loaded = Artifacts::load(&root).unwrap();
        assert_eq!(loaded.libs, artifacts.libs);
        assert_eq!(loaded.system_libs, artifacts.system_libs);
        assert_eq!(loaded.resource_dir, artifacts.resource_dir);
        assert_eq!(loaded.target, artifacts.target);
        util::remove_dir_all(&root).unwrap();
    }
}
