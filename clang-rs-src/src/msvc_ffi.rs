//! Builds libffi for MSVC targets with the `cc` crate.
//!
//! libffi's autotools build only supports MSVC through the `msvcc.sh` wrapper
//! (which needs a Unix shell), so this mirrors what that wrapper does: the
//! headers are generated directly, the C sources are compiled with `cl.exe`,
//! and the assembly sources are run through the C preprocessor (`cl -EP`)
//! before being assembled with `ml64.exe` / `armasm64.exe`.

use std::fs;
use std::path::Path;
use std::process::Stdio;

use crate::autotools::InstalledLib;
use crate::source::LIBFFI;
use crate::target::TargetInfo;
use crate::toolchain::Toolchain;
use crate::util::{self, Result};

const COMMON_SOURCES: &[&str] = &[
    "prep_cif.c",
    "types.c",
    "raw_api.c",
    "java_raw_api.c",
    "closures.c",
    "tramp.c",
];

pub(crate) fn build_libffi(
    src: &Path,
    work: &Path,
    target: &TargetInfo,
    toolchain: &Toolchain,
) -> Result<InstalledLib> {
    // (source dir, ffi target macro, C sources, assembly source)
    let (dir, target_macro, c_sources, asm) = match target.rust_arch() {
        "x86_64" => ("x86", "X86_WIN64", &["ffiw64.c"][..], "win64_intel.S"),
        "aarch64" => ("aarch64", "ARM_WIN64", &["ffi.c"][..], "win64_armasm.S"),
        arch => {
            return Err(format!(
                "building libffi for `{arch}` MSVC targets is not supported"
            ))
        }
    };

    let build_dir = work.join("libffi-build");
    let prefix = work.join("libffi");
    util::remove_dir_all(&build_dir)?;
    util::remove_dir_all(&prefix)?;
    let gen_include = build_dir.join("include");
    util::create_dir_all(&gen_include)?;

    let src_include = src.join("include");
    let arch_dir = src.join("src").join(dir);

    util::write(&gen_include.join("ffi.h"), ffi_h(src, target_macro)?)?;
    util::write(&gen_include.join("fficonfig.h"), fficonfig_h())?;
    util::link_or_copy(
        &arch_dir.join("ffitarget.h"),
        &gen_include.join("ffitarget.h"),
    )?;

    // Preprocess the assembly into something ml64/armasm64 understand.
    let asm_out = build_dir.join(asm.replace(".S", ".asm"));
    let asm_file = fs::File::create(&asm_out).map_err(|e| format!("{}: {e}", asm_out.display()))?;
    let mut preprocess = toolchain.c.to_command();
    preprocess
        .arg("-nologo")
        .arg("-EP")
        .arg(format!("-I{}", gen_include.display()))
        .arg(format!("-I{}", src_include.display()))
        .arg(format!("-I{}", arch_dir.display()))
        .arg("-DFFI_STATIC_BUILD")
        // Treat the `.S` file as C so that it gets preprocessed.
        .arg("-TC")
        .arg(arch_dir.join(asm))
        .stdout(Stdio::from(asm_file));
    util::run(&mut preprocess, "preprocessing libffi assembly")?;

    let mut build = toolchain.cc_build.clone();
    build
        .cargo_metadata(false)
        .out_dir(build_dir.join("out"))
        .include(&gen_include)
        .include(&src_include)
        .include(&arch_dir)
        .define("FFI_STATIC_BUILD", None)
        .define("_CRT_SECURE_NO_DEPRECATE", None)
        .warnings(false);
    for file in COMMON_SOURCES {
        build.file(src.join("src").join(file));
    }
    for file in c_sources {
        build.file(arch_dir.join(file));
    }
    build.file(&asm_out);
    build
        .try_compile("ffi")
        .map_err(|e| format!("failed to compile libffi: {e}"))?;

    let lib_dir = prefix.join("lib");
    let include_dir = prefix.join("include");
    util::create_dir_all(&lib_dir)?;
    util::create_dir_all(&include_dir)?;
    util::link_or_copy(
        &build_dir.join("out").join("ffi.lib"),
        &lib_dir.join("ffi.lib"),
    )?;
    for header in ["ffi.h", "ffitarget.h"] {
        util::link_or_copy(&gen_include.join(header), &include_dir.join(header))?;
    }
    Ok(InstalledLib {
        include_dir,
        lib: lib_dir.join("ffi.lib"),
    })
}

/// Generates `ffi.h` from `ffi.h.in` the way `configure` would for MSVC.
fn ffi_h(src: &Path, target_macro: &str) -> Result<String> {
    let template = util::read_to_string(&src.join("include").join("ffi.h.in"))?;
    ffi_h_from_template(&template, target_macro)
}

fn ffi_h_from_template(template: &str, target_macro: &str) -> Result<String> {
    let version = LIBFFI.version;
    let number = version
        .split('.')
        .map(|p| p.parse::<u32>().unwrap_or(0))
        .chain(std::iter::repeat(0))
        .take(3)
        .fold(0, |acc, p| acc * 100 + p);
    let header = template
        .replace("@VERSION@", version)
        .replace("@TARGET@", target_macro)
        // MSVC's `long double` is the same type as `double`.
        .replace("@HAVE_LONG_DOUBLE@", "0")
        .replace("@HAVE_LONG_DOUBLE_VARIANT@", "0")
        .replace("@FFI_EXEC_TRAMPOLINE_TABLE@", "0")
        .replace("@FFI_VERSION_STRING@", version)
        .replace("@FFI_VERSION_NUMBER@", &number.to_string());
    if let Some(name) = find_placeholder(&header) {
        return Err(format!("unhandled substitution {name} in ffi.h.in"));
    }
    // This is a static library: make the header say so for every consumer
    // so that MSVC does not reference `__imp_` symbols.
    Ok(format!(
        "#ifndef FFI_STATIC_BUILD\n#define FFI_STATIC_BUILD\n#endif\n{header}"
    ))
}

/// Finds an autoconf placeholder such as `@TARGET@`.
fn find_placeholder(s: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(offset) = s[from..].find('@') {
        let start = from + offset;
        let rest = &s.as_bytes()[start + 1..];
        let len = rest
            .iter()
            .take_while(|b| b.is_ascii_uppercase() || **b == b'_')
            .count();
        if len > 0 && rest.get(len) == Some(&b'@') {
            return Some(&s[start..start + len + 2]);
        }
        from = start + 1;
    }
    None
}

fn fficonfig_h() -> String {
    let version = LIBFFI.version;
    format!(
        r#"/* fficonfig.h for MSVC targets, generated by clang-rs-src. */
#define HAVE_ALLOCA 1
#define HAVE_INTTYPES_H 1
#define HAVE_MEMCPY 1
#define HAVE_MEMORY_H 1
#define HAVE_STDINT_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STRING_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TYPES_H 1
#define STDC_HEADERS 1
#define SIZEOF_DOUBLE 8
#define SIZEOF_LONG_DOUBLE 8
#define SIZEOF_SIZE_T 8
#define PACKAGE "libffi"
#define PACKAGE_BUGREPORT "http://github.com/libffi/libffi/issues"
#define PACKAGE_NAME "libffi"
#define PACKAGE_STRING "libffi {version}"
#define PACKAGE_TARNAME "libffi"
#define PACKAGE_URL ""
#define PACKAGE_VERSION "{version}"
#define VERSION "{version}"

#ifdef LIBFFI_ASM
#define FFI_HIDDEN(name)
#else
#define FFI_HIDDEN
#endif
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_ffi_h() {
        // The substitutions of libffi 3.8.0's include/ffi.h.in.
        let template = "/* libffi @VERSION@ */\n#ifndef @TARGET@\n#define @TARGET@\n#endif\n\
                        #if @HAVE_LONG_DOUBLE@\n#endif\n#if @FFI_EXEC_TRAMPOLINE_TABLE@\n#endif\n\
                        #define FFI_VERSION_STRING \"@FFI_VERSION_STRING@\"\n\
                        #define FFI_VERSION_NUMBER @FFI_VERSION_NUMBER@\n";
        let h = ffi_h_from_template(template, "X86_WIN64").unwrap();
        assert!(h.starts_with("#ifndef FFI_STATIC_BUILD"));
        assert!(h.contains("#ifndef X86_WIN64"));
        assert!(h.contains("#define FFI_VERSION_NUMBER 30800"));
        assert!(h.contains("#define FFI_VERSION_STRING \"3.8.0\""));
        assert!(!h.contains('@'));
        assert!(ffi_h_from_template("#if @NEW_SUBSTITUTION@", "X86_WIN64").is_err());
    }

    #[test]
    fn placeholders() {
        assert_eq!(find_placeholder("a @TARGET@ b"), Some("@TARGET@"));
        assert_eq!(find_placeholder("mail@example.com @ x@Y_Z@"), Some("@Y_Z@"));
        assert_eq!(find_placeholder("no placeholders @ here @"), None);
    }
}
