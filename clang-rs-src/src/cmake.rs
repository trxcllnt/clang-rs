//! A small CMake driver.
//!
//! This doesn't use the `cmake` crate because building LLVM needs things it
//! doesn't offer: building several specific targets, two independent
//! configurations (host tools and target libraries) in one build script,
//! keeping CMake's per-configuration optimization flags (the `cmake` crate
//! overrides `CMAKE_<LANG>_FLAGS_RELEASE` for Visual Studio generators), and
//! reporting failures as errors rather than panics.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::target::TargetInfo;
use crate::toolchain::{join, Toolchain};
use crate::util::{self, Result};

pub(crate) struct CMake<'a> {
    pub src: PathBuf,
    pub build_dir: PathBuf,
    pub target: &'a TargetInfo,
    pub host: &'a TargetInfo,
    pub toolchain: &'a Toolchain,
    /// Set `CMAKE_SYSTEM_NAME` & co. (i.e. this is a cross build).
    pub cross: bool,
    pub toolchain_file: Option<String>,
    pub static_crt: bool,
    pub defines: Vec<(String, String)>,
    pub env: Vec<(OsString, OsString)>,
    pub env_remove: Vec<&'static str>,
    pub jobs: usize,
}

const STAMP: &str = "clang-rs-src-configure.stamp";

impl CMake<'_> {
    pub fn define(&mut self, key: &str, value: impl Into<String>) -> &mut Self {
        let value = value.into();
        match self.defines.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.defines.push((key.to_string(), value)),
        }
        self
    }

    pub fn define_path(&mut self, key: &str, path: &Path) -> &mut Self {
        self.define(key, util::cmake_path(path))
    }

    fn program() -> OsString {
        env::var_os("CMAKE").unwrap_or_else(|| "cmake".into())
    }

    /// The CMake generator, or `None` for CMake's default.
    fn generator(&self) -> Option<String> {
        if let Some(g) = util::target_env("CMAKE_GENERATOR", &self.target.triple, &self.host.triple)
        {
            return Some(g);
        }
        // CMake's default for MSVC (the newest Visual Studio) sets up the
        // compiler environment, resource compiler and manifest tool by
        // itself, whereas Ninja needs a Developer Command Prompt for those.
        if cfg!(windows) && self.target.is_msvc() {
            return None;
        }
        if util::probe("ninja".as_ref(), "--version") {
            return Some("Ninja".to_string());
        }
        None
    }

    fn is_visual_studio(&self, generator: Option<&str>) -> bool {
        match generator {
            Some(g) => g.starts_with("Visual Studio"),
            None => self.target.is_msvc() && cfg!(windows),
        }
    }

    fn apply_env(&self, cmd: &mut Command, visual_studio: bool) {
        // MSBuild finds the MSVC tools itself; everything else gets the
        // environment (INCLUDE, LIB, PATH, ...) that `cc` determined.
        if !visual_studio {
            for (k, v) in self.toolchain.c.env() {
                cmd.env(k, v);
            }
        }
        for k in &self.env_remove {
            cmd.env_remove(k);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
    }

    /// The full configure command line.
    fn configure_command(&self) -> Command {
        let generator = self.generator();
        let vs = self.is_visual_studio(generator.as_deref());
        let tc = self.toolchain;

        let mut cmd = Command::new(Self::program());
        cmd.arg("-S")
            .arg(&self.src)
            .arg("-B")
            .arg(&self.build_dir)
            .arg("--no-warn-unused-cli");
        if let Some(g) = &generator {
            cmd.arg("-G").arg(g);
        }
        if vs {
            if let Some(platform) = self.target.msvc_platform() {
                cmd.arg("-A").arg(platform);
            }
            // The default 32-bit host toolset runs out of memory on LLVM.
            match self.host.rust_arch() {
                "x86_64" => {
                    cmd.arg("-Thost=x64");
                }
                "aarch64" => {
                    cmd.arg("-Thost=ARM64");
                }
                _ => {}
            }
        }

        let mut defines: Vec<(String, String)> = Vec::new();
        let mut def = |k: &str, v: String| defines.push((k.to_string(), v));
        def("CMAKE_BUILD_TYPE", "Release".into());
        def(
            "CMAKE_INSTALL_PREFIX",
            util::cmake_path(&self.build_dir.join("install")),
        );
        if let Some(file) = &self.toolchain_file {
            def("CMAKE_TOOLCHAIN_FILE", file.clone());
        } else {
            if self.cross {
                def("CMAKE_SYSTEM_NAME", self.target.cmake_system_name().into());
                def(
                    "CMAKE_SYSTEM_PROCESSOR",
                    self.target.cmake_system_processor().into(),
                );
            }
            if !vs {
                def(
                    "CMAKE_C_COMPILER",
                    util::cmake_path(&absolute_tool(tc.c.path())),
                );
                def(
                    "CMAKE_CXX_COMPILER",
                    util::cmake_path(&absolute_tool(tc.cxx.path())),
                );
                if let Some(ar) = &tc.explicit_ar {
                    def("CMAKE_AR", util::cmake_path(&absolute_tool(Path::new(ar))));
                }
                if let Some(ranlib) = &tc.explicit_ranlib {
                    def(
                        "CMAKE_RANLIB",
                        util::cmake_path(&absolute_tool(Path::new(ranlib))),
                    );
                }
            }
        }
        // Setting CMAKE_<LANG>_FLAGS replaces CMake's initial flags, so keep
        // the ones it uses for MSVC (LLVM adjusts /EH and /GR itself).
        let (c_init, cxx_init) = if self.target.is_msvc() {
            ("/DWIN32 /D_WINDOWS ", "/DWIN32 /D_WINDOWS /EHsc ")
        } else {
            ("", "")
        };
        def(
            "CMAKE_C_FLAGS",
            format!("{c_init}{}", join(&tc.c_flags).to_string_lossy()),
        );
        def(
            "CMAKE_CXX_FLAGS",
            format!("{cxx_init}{}", join(&tc.cxx_flags).to_string_lossy()),
        );
        if self.target.is_apple() {
            def("CMAKE_OSX_ARCHITECTURES", self.target.apple_arch().into());
        }
        if self.target.is_msvc() {
            let runtime = if self.static_crt {
                "MultiThreaded"
            } else {
                "MultiThreadedDLL"
            };
            def("CMAKE_MSVC_RUNTIME_LIBRARY", runtime.into());
        } else if !self.target.is_windows() {
            def("CMAKE_POSITION_INDEPENDENT_CODE", "ON".into());
        }
        for (k, v) in defines.into_iter().chain(self.defines.iter().cloned()) {
            cmd.arg(format!("-D{k}={v}"));
        }
        self.apply_env(&mut cmd, vs);
        cmd
    }

    /// Configures the build tree unless it is already configured with exactly
    /// the same settings.
    pub fn configure(&self) -> Result<()> {
        let mut cmd = self.configure_command();
        let fingerprint = util::sha256_str(&format!(
            "{:?} {:?} {:?}",
            cmd.get_program(),
            cmd.get_args().collect::<Vec<_>>(),
            cmd.get_envs().collect::<Vec<_>>()
        ));
        let stamp = self.build_dir.join(STAMP);
        let up_to_date = self.build_dir.join("CMakeCache.txt").exists()
            && std::fs::read_to_string(&stamp).is_ok_and(|s| s == fingerprint);
        if up_to_date {
            println!("{} is already configured", self.build_dir.display());
            return Ok(());
        }
        check_version()?;
        util::remove_dir_all(&self.build_dir)?;
        util::create_dir_all(&self.build_dir)?;
        util::run(&mut cmd, "configuring LLVM with CMake").map_err(|e| {
            format!(
                "{e}\n\nSee {} for details. Building LLVM requires CMake >= 3.20, Python >= 3.8 \
                 and a C++17 compiler for both the host and the target.",
                self.build_dir
                    .join("CMakeFiles")
                    .join("CMakeConfigureLog.yaml")
                    .display()
            )
        })?;
        util::write(&stamp, fingerprint)
    }

    pub fn build(&self, targets: &[&str]) -> Result<()> {
        let mut cmd = Command::new(Self::program());
        cmd.arg("--build")
            .arg(&self.build_dir)
            .arg("--config")
            .arg("Release")
            .arg("--target")
            .args(targets);
        let makefiles = self.build_dir.join("Makefile").exists();
        match env::var_os("CARGO_MAKEFLAGS") {
            // GNU make can share Cargo's jobserver (reliably only on Linux).
            Some(flags) if makefiles && cfg!(target_os = "linux") => {
                cmd.env("MAKEFLAGS", flags);
            }
            _ => {
                cmd.arg("--parallel").arg(self.jobs.to_string());
            }
        }
        let vs = self.is_visual_studio(self.generator().as_deref());
        self.apply_env(&mut cmd, vs);
        util::run(&mut cmd, &format!("building {}", targets.join(" ")))
    }

    /// Directory containing the executables of this build.
    pub fn bin_dir(&self) -> PathBuf {
        let release = self.build_dir.join("Release").join("bin");
        if release.is_dir() {
            release
        } else {
            self.build_dir.join("bin")
        }
    }
}

/// CMake wants absolute compiler paths; resolve bare names through `PATH`.
fn absolute_tool(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    path.to_str()
        .and_then(util::find_on_path)
        .unwrap_or_else(|| path.to_path_buf())
}

fn check_version() -> Result<()> {
    let out = util::output(Command::new(CMake::program()).arg("--version"))
        .map_err(|e| format!("CMake is required to build LLVM but could not be run: {e}"))?;
    let version = out
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("cmake version "))
        .unwrap_or("");
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    if (major, minor) < (3, 20) {
        return Err(format!(
            "CMake >= 3.20 is required to build LLVM, found `{version}`"
        ));
    }
    Ok(())
}
