//! Builds libffi and ncurses with their autotools build systems (all
//! non-MSVC targets).

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::target::TargetInfo;
use crate::toolchain::{join, Toolchain};
use crate::util::{self, Result};

/// A `configure && make && make install` invocation.
pub(crate) struct ConfigureMake<'a> {
    pub name: &'static str,
    pub src: &'a Path,
    pub build_dir: PathBuf,
    pub prefix: PathBuf,
    pub target: &'a TargetInfo,
    pub host: &'a TargetInfo,
    pub toolchain: &'a Toolchain,
    pub args: Vec<OsString>,
    pub env: Vec<(&'static str, OsString)>,
    pub jobs: usize,
}

impl ConfigureMake<'_> {
    pub fn run(&self) -> Result<()> {
        util::remove_dir_all(&self.build_dir)?;
        util::remove_dir_all(&self.prefix)?;
        util::create_dir_all(&self.build_dir)?;

        let tc = self.toolchain;
        let mut cflags = vec![OsString::from("-O2")];
        cflags.extend(tc.c_flags.iter().cloned());

        let mut configure = Command::new("sh");
        configure
            .arg(self.src.join("configure"))
            .arg(format!("--prefix={}", self.prefix.display()))
            .arg(format!("--libdir={}", self.prefix.join("lib").display()))
            .arg(format!(
                "--includedir={}",
                self.prefix.join("include").display()
            ))
            .arg(format!("--build={}", self.host.gnu_triple()))
            .arg(format!("--host={}", self.target.gnu_triple()))
            .args(&self.args)
            .current_dir(&self.build_dir)
            .env("CC", tc.cc_env())
            .env("CFLAGS", join(&cflags))
            .env("AR", &tc.ar)
            .env("RANLIB", &tc.ranlib)
            // `cc` honours CROSS_COMPILE already; don't let make double-prefix.
            .env_remove("CROSS_COMPILE")
            .env_remove("CPPFLAGS")
            .env_remove("LDFLAGS")
            .env_remove("LIBS");
        for (k, v) in &self.env {
            configure.env(k, v);
        }
        for (k, v) in tc.c.env() {
            configure.env(k, v);
        }
        util::run(&mut configure, &format!("configuring {}", self.name))?;

        let mut make = self.make();
        make.current_dir(&self.build_dir);
        util::run(&mut make, &format!("building {}", self.name))?;

        let mut install = self.make();
        install.arg("install").current_dir(&self.build_dir);
        util::run(&mut install, &format!("installing {}", self.name))
    }

    fn make(&self) -> Command {
        let program = env::var_os("MAKE").unwrap_or_else(|| {
            let bsd = [
                "freebsd",
                "openbsd",
                "netbsd",
                "dragonfly",
                "solaris",
                "illumos",
            ];
            if bsd.iter().any(|os| self.host.os.starts_with(os)) {
                "gmake".into()
            } else {
                "make".into()
            }
        });
        let mut cmd = Command::new(program);
        // Share Cargo's jobserver on Linux; elsewhere the inherited file
        // descriptors and older makes (macOS ships GNU make 3.81) are not
        // reliable, so use -j.
        match env::var_os("CARGO_MAKEFLAGS") {
            Some(flags) if cfg!(target_os = "linux") => {
                cmd.env("MAKEFLAGS", flags);
            }
            _ => {
                cmd.arg(format!("-j{}", self.jobs));
            }
        }
        cmd.env_remove("CROSS_COMPILE");
        cmd
    }
}

/// Paths of an installed static library and its headers.
#[derive(Clone, Debug)]
pub(crate) struct InstalledLib {
    pub include_dir: PathBuf,
    pub lib: PathBuf,
}

pub(crate) fn build_libffi(
    src: &Path,
    work: &Path,
    target: &TargetInfo,
    host: &TargetInfo,
    toolchain: &Toolchain,
    jobs: usize,
) -> Result<InstalledLib> {
    let prefix = work.join("libffi");
    ConfigureMake {
        name: "libffi",
        src,
        build_dir: work.join("libffi-build"),
        prefix: prefix.clone(),
        target,
        host,
        toolchain,
        args: [
            "--disable-shared",
            "--enable-static",
            "--with-pic",
            "--disable-docs",
            "--disable-multi-os-directory",
            "--disable-dependency-tracking",
        ]
        .iter()
        .map(OsString::from)
        .collect(),
        env: Vec::new(),
        jobs,
    }
    .run()?;
    Ok(InstalledLib {
        include_dir: prefix.join("include"),
        lib: prefix.join("lib").join("libffi.a"),
    })
}

pub(crate) fn build_ncurses(
    src: &Path,
    work: &Path,
    target: &TargetInfo,
    host: &TargetInfo,
    toolchain: &Toolchain,
    host_toolchain: &Toolchain,
    jobs: usize,
) -> Result<InstalledLib> {
    let prefix = work.join("ncurses");
    // Helper programs that generate sources run on the build machine, so they
    // must be compiled with the host compiler.
    let mut build_cc = OsString::from("--with-build-cc=");
    build_cc.push(host_toolchain.cc_env());
    let args = vec![
        build_cc,
        // Static, position independent, wide-character libncursesw only.
        "--without-shared".into(),
        "--with-normal".into(),
        "--without-debug".into(),
        "--without-profile".into(),
        "--enable-widec".into(),
        "--without-cxx".into(),
        "--without-cxx-binding".into(),
        "--without-ada".into(),
        "--without-manpages".into(),
        "--without-progs".into(),
        "--without-tests".into(),
        "--without-pkg-config".into(),
        "--disable-db-install".into(),
        "--without-gpm".into(),
        "--enable-overwrite".into(),
        "--disable-stripping".into(),
        // The terminfo database of the machine the final binary runs on is
        // used at runtime; search the locations used by common distributions
        // and macOS.
        "--with-terminfo-dirs=/etc/terminfo:/lib/terminfo:/usr/share/terminfo:/usr/lib/terminfo"
            .into(),
        "--with-default-terminfo-dir=/usr/share/terminfo".into(),
    ];
    ConfigureMake {
        name: "ncurses",
        src,
        build_dir: work.join("ncurses-build"),
        prefix: prefix.clone(),
        target,
        host,
        toolchain,
        args,
        env: Vec::new(),
        jobs,
    }
    .run()?;
    Ok(InstalledLib {
        include_dir: prefix.join("include"),
        lib: prefix.join("lib").join("libncursesw.a"),
    })
}
