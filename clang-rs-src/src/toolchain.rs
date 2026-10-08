//! C/C++ toolchain discovery (via the `cc` crate) for the host and target.

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::target::TargetInfo;
use crate::util::{self, Result};

/// How the C++ standard library must be linked into the final artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CxxStdlib {
    /// Nothing to do (MSVC picks its runtime through `/DEFAULTLIB` directives).
    None,
    /// Link `-l<name>` dynamically.
    Dylib(String),
    /// Link this static archive (copied next to the other libraries).
    Static(PathBuf),
}

/// Compilers and tools for one target, as configured through the usual `cc`
/// crate environment variables (`CC_<target>`, `CXX_<target>`, `AR_<target>`,
/// `CFLAGS_<target>`, ...).
pub(crate) struct Toolchain {
    pub c: cc::Tool,
    pub cxx: cc::Tool,
    pub ar: OsString,
    pub ranlib: OsString,
    /// C flags with optimization, debug-info and CRT-selection flags removed.
    pub c_flags: Vec<OsString>,
    pub cxx_flags: Vec<OsString>,
    /// An archiver explicitly configured by the user (`AR_<target>` etc.).
    pub explicit_ar: Option<String>,
    pub explicit_ranlib: Option<String>,
    /// The `cc::Build` used to probe the C compiler; reused to compile libffi
    /// on MSVC.
    pub cc_build: cc::Build,
}

impl Toolchain {
    pub fn new(
        target: &TargetInfo,
        host: &TargetInfo,
        static_crt: Option<bool>,
    ) -> Result<Toolchain> {
        let mut build = cc::Build::new();
        build
            .target(&target.triple)
            .host(&host.triple)
            .opt_level(2)
            .debug(false)
            .warnings(false)
            .pic(!target.is_windows());
        if let Some(static_crt) = static_crt {
            build.static_crt(static_crt);
        }

        let tenv = |var: &str| util::target_env(var, &target.triple, &host.triple);
        let explicit_ar = tenv("AR");
        let explicit_ranlib = tenv("RANLIB");

        // The `cc` crate falls back to the host compiler for cross targets it
        // has no prefix for (e.g. `loongarch64-unknown-linux-musl`). Look for a
        // conventionally named GCC cross toolchain instead.
        let mut cxx_override = None;
        let user_configured = tenv("CC").is_some()
            || tenv("CXX").is_some()
            || env::var_os("CROSS_COMPILE").is_some()
            || env::var_os("RUSTC_LINKER").is_some();
        if target.triple != host.triple && !user_configured {
            for prefix in target.cross_prefixes() {
                let (Some(gcc), Some(gxx)) = (
                    util::find_on_path(&format!("{prefix}-gcc")),
                    util::find_on_path(&format!("{prefix}-g++")),
                ) else {
                    continue;
                };
                println!("using cross toolchain {prefix}-gcc for {}", target.triple);
                build.compiler(gcc);
                cxx_override = Some(gxx);
                if explicit_ar.is_none() {
                    if let Some(ar) = util::find_on_path(&format!("{prefix}-ar")) {
                        build.archiver(ar);
                    }
                }
                if explicit_ranlib.is_none() {
                    if let Some(ranlib) = util::find_on_path(&format!("{prefix}-ranlib")) {
                        build.ranlib(ranlib);
                    }
                }
                break;
            }
        }

        let c = build
            .try_get_compiler()
            .map_err(|e| format!("failed to find a C compiler for {}: {e}", target.triple))?;
        let mut cxx_build = build.clone();
        cxx_build.cpp(true);
        if let Some(gxx) = cxx_override {
            cxx_build.compiler(gxx);
        }
        let cxx = cxx_build
            .try_get_compiler()
            .map_err(|e| format!("failed to find a C++ compiler for {}: {e}", target.triple))?;

        // Report a missing cross toolchain here rather than as obscure
        // configure or assembler errors later on.
        let target_u = target.triple.replace('-', "_");
        for tool in [&c, &cxx] {
            let path = tool.path();
            if path.is_relative() && path.to_str().and_then(util::find_on_path).is_none() {
                return Err(format!(
                    "the compiler `{}` selected for {} was not found; install a toolchain \
                     for the target or set CC_{target_u} and CXX_{target_u}",
                    path.display(),
                    target.triple,
                ));
            }
        }
        // A plain `cc`/`gcc` is a GCC for the build machine, which (unlike
        // Clang) cannot generate code for another architecture.
        let host_gcc = matches!(
            c.path().file_stem().and_then(|s| s.to_str()),
            Some("cc" | "gcc")
        ) && !c.is_like_clang();
        if target.rust_arch() != host.rust_arch() && !user_configured && host_gcc {
            let hint = target
                .cross_prefixes()
                .first()
                .map(|p| format!(" (e.g. put {p}-gcc and {p}-g++ on PATH)"))
                .unwrap_or_default();
            return Err(format!(
                "no C/C++ cross compiler for {} was found: `{}` generates code for the build \
                 machine. Install a cross toolchain{hint} or set CC_{target_u}, CXX_{target_u} \
                 and AR_{target_u}",
                target.triple,
                c.path().display(),
            ));
        }

        let ar = build.get_archiver().get_program().to_os_string();
        let ranlib = build.get_ranlib().get_program().to_os_string();
        let c_flags = filter_flags(c.args());
        let cxx_flags = filter_flags(cxx.args());

        Ok(Toolchain {
            c,
            cxx,
            ar,
            ranlib,
            c_flags,
            cxx_flags,
            explicit_ar,
            explicit_ranlib,
            cc_build: build,
        })
    }

    /// The value for `CC` in an autotools environment (keeping a compiler
    /// wrapper such as `sccache` if one was configured).
    pub fn cc_env(&self) -> OsString {
        let env = self.c.cc_env();
        if env.is_empty() {
            self.c.path().as_os_str().to_os_string()
        } else {
            env
        }
    }

    /// Determines how the C++ standard library used by the C++ compiler has to
    /// be linked into the final binary.
    pub fn cxx_stdlib(&self, target: &TargetInfo, host: &TargetInfo) -> CxxStdlib {
        if let Some(lib) = util::target_env("CXXSTDLIB", &target.triple, &host.triple) {
            return if lib.is_empty() {
                CxxStdlib::None
            } else {
                CxxStdlib::Dylib(lib)
            };
        }
        if target.is_msvc() {
            return CxxStdlib::None;
        }
        let uses_libcxx = self
            .cxx_flags
            .iter()
            .any(|f| f == OsStr::new("-stdlib=libc++"));
        if target.is_apple() || uses_libcxx {
            return CxxStdlib::Dylib("c++".to_string());
        }
        let static_requested = env::var("CLANG_RS_SRC_STATIC_CXX_STDLIB").is_ok_and(|v| v == "1");
        if target.is_musl() || static_requested {
            if let Some(path) = self.print_file_name("libstdc++.a") {
                return CxxStdlib::Static(path);
            }
            println!(
                "cargo:warning=clang-rs-src: could not locate libstdc++.a for {}; \
                 linking -lstdc++ instead",
                target.triple
            );
        }
        CxxStdlib::Dylib("stdc++".to_string())
    }

    /// Asks the C++ compiler driver where it would find a library file.
    fn print_file_name(&self, file: &str) -> Option<PathBuf> {
        let mut cmd = self.cxx.to_command();
        cmd.arg(format!("-print-file-name={file}"));
        let out = util::output(&mut cmd).ok()?;
        let path = PathBuf::from(out);
        (path.is_absolute() && path.is_file()).then(|| canonical(&path))
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Removes flags that the respective build systems manage themselves.
fn filter_flags(args: &[OsString]) -> Vec<OsString> {
    args.iter()
        .filter(|arg| {
            let s = arg.to_string_lossy();
            let optimization = s.starts_with("-O") || s.starts_with("/O");
            let debug_info = s.starts_with("-g") && !s.starts_with("-gcc");
            let msvc_crt = matches!(
                &*s,
                "-MD" | "-MT" | "-MDd" | "-MTd" | "/MD" | "/MT" | "/MDd" | "/MTd"
            );
            // `cc` adds `-static` for `crt-static` targets; it is a link-time
            // flag that breaks configure checks and shared helper programs.
            let link_static = s == "-static";
            !(optimization || debug_info || msvc_crt || link_static)
        })
        .cloned()
        .collect()
}

pub(crate) fn join(flags: &[OsString]) -> OsString {
    let mut out = OsString::new();
    for (i, flag) in flags.iter().enumerate() {
        if i > 0 {
            out.push(" ");
        }
        out.push(flag);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::filter_flags;
    use std::ffi::OsString;

    #[test]
    fn filters_managed_flags() {
        let args: Vec<OsString> = [
            "-O2",
            "-ffunction-sections",
            "-g",
            "-gdwarf-4",
            "-fPIC",
            "-static",
            "-MD",
            "-nologo",
            "--target=arm64-apple-macosx11.0",
            "-gcc-toolchain",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        let kept: Vec<_> = filter_flags(&args)
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        assert_eq!(
            kept,
            [
                "-ffunction-sections",
                "-fPIC",
                "-nologo",
                "--target=arm64-apple-macosx11.0",
                "-gcc-toolchain"
            ]
        );
    }
}
