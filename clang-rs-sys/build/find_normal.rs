use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Links an existing (usually system-wide) libclang dynamically.
///
/// The library is looked up in `CLANG_RS_SYS_LIB_DIR`, then `LIBCLANG_PATH`,
/// then the `--libdir` reported by `llvm-config` (`LLVM_CONFIG_PATH`), and
/// finally the linker's default search path.
///
/// Without a version feature, the crate declares the API of that library's
/// version: it is detected, and the `feature` cfgs of its `clang_<major>_0`
/// feature and all older ones are set. (A build script cannot enable Cargo
/// features, but the bindings only look at the cfgs.)
pub fn link() {
    let target = env::var("TARGET").expect("TARGET is set by Cargo");
    let llvm_config = env::var_os("LLVM_CONFIG_PATH").unwrap_or_else(|| "llvm-config".into());

    let mut llvm_config_version = None;
    let lib_dir = ["CLANG_RS_SYS_LIB_DIR", "LIBCLANG_PATH"]
        .iter()
        .find_map(|var| env::var_os(var).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .or_else(|| {
            let dir = output_of(&llvm_config, "--libdir")?;
            llvm_config_version = output_of(&llvm_config, "--version");
            Some(PathBuf::from(dir))
        });

    if let Some(dir) = &lib_dir {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:lib={}", dir.display());
    }

    // The import library of `libclang.dll` is `libclang.lib`.
    let name = if target.contains("windows-msvc") {
        "libclang"
    } else {
        "clang"
    };
    println!("cargo:rustc-link-lib=dylib={name}");

    let library = find_library(&target, lib_dir.as_deref());
    if let Some(library) = &library {
        // Detect the version again when the library is replaced.
        println!("cargo:rerun-if-changed={}", library.display());
    }
    let detected = detect_version(
        &target,
        lib_dir.as_deref(),
        library.as_deref(),
        llvm_config_version.as_deref(),
    );
    declare_api(detected);
}

/// The trimmed output of `program arg`, if it succeeds.
fn output_of(program: impl AsRef<OsStr>, arg: &str) -> Option<String> {
    let output = Command::new(program).arg(arg).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
}

/// The library file that is linked, if it can be found.
fn find_library(target: &str, lib_dir: Option<&Path>) -> Option<PathBuf> {
    let file_name = if target.contains("windows-msvc") {
        "libclang.lib"
    } else if target.contains("apple") {
        "libclang.dylib"
    } else {
        "libclang.so"
    };
    if let Some(dir) = lib_dir {
        return Some(dir.join(file_name)).filter(|path| path.exists());
    }
    // In the default search path of the C compiler, which rustc links with.
    // Only for native builds, as `cc` is the host's compiler.
    if env::var("HOST").ok()? != target || target.contains("windows") || target.contains("apple") {
        return None;
    }
    // Prints the name unchanged if it doesn't find the file.
    let path = PathBuf::from(output_of("cc", &format!("-print-file-name={file_name}"))?);
    Some(path).filter(|path| path.is_absolute())
}

struct Detected {
    /// As precise as known: `19.1.1`, or just `19`.
    version: String,
    major: u32,
}

/// The version of the libclang that is linked, or why it is unknown.
fn detect_version(
    target: &str,
    lib_dir: Option<&Path>,
    library: Option<&Path>,
    llvm_config_version: Option<&str>,
) -> Result<Detected, String> {
    let dir = match (lib_dir, library.and_then(Path::parent)) {
        (Some(dir), _) | (None, Some(dir)) => dir,
        (None, None) => return Err("it is not in a configured directory".into()),
    };
    if target.contains("apple") && is_apple_libclang(dir) {
        return Err(format!(
            "{} is Apple's libclang, whose versions don't correspond to LLVM releases",
            dir.display()
        ));
    }
    llvm_config_version
        .and_then(parse_version)
        .or_else(|| file_name_version(library?))
        .or_else(|| resource_dir_version(dir))
        .ok_or_else(|| {
            format!(
                "neither the library's file name nor a resource directory in {} tells it",
                dir.display()
            )
        })
}

/// Whether `dir` is part of Xcode or its Command Line Tools.
fn is_apple_libclang(dir: &Path) -> bool {
    let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let dir = dir.to_string_lossy();
    dir.contains("/CommandLineTools/") || dir.contains("/Xcode")
}

/// The `major[.minor[.patch]]` at the start of `s`, such as the `19.1.1` of
/// `19.1.1git` or the `19` of `19.so.19`.
fn parse_version(s: &str) -> Option<Detected> {
    let end = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let version = s[..end].trim_end_matches('.');
    let major = version.split('.').next()?.parse().ok()?;
    // Such as the ABI version in `libclang.so.1`, not an LLVM version.
    if major < 3 {
        return None;
    }
    Some(Detected {
        version: version.to_string(),
        major,
    })
}

/// The version in the name of the file a libclang symlink points to, such as
/// `libclang-19.so.19` (Debian and Ubuntu) or `libclang.so.23.1.3` (LLVM's
/// own build).
fn file_name_version(library: &Path) -> Option<Detected> {
    let library = fs::canonicalize(library).ok()?;
    let rest = library.file_name()?.to_str()?.strip_prefix("libclang")?;
    match rest.strip_prefix('-') {
        Some(versioned) => parse_version(versioned),
        None => parse_version(rest.strip_prefix(".so.")?),
    }
}

/// The version of the resource directory (with Clang's builtin headers) that
/// libclang finds at `<library dir>/../lib/clang/<version>`. It is named
/// after the major version since LLVM 16 and the full version before.
fn resource_dir_version(dir: &Path) -> Option<Detected> {
    let mut found = BTreeMap::new();
    for clang in [dir.join("clang"), dir.join("../lib/clang")] {
        for entry in fs::read_dir(clang).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(version) = parse_version(name).filter(|v| v.version == name) {
                found.insert(version.major, version);
            }
        }
    }
    // Several would be ambiguous.
    if found.len() == 1 {
        found.into_values().next()
    } else {
        None
    }
}

/// Declares the API of the detected version if no version feature is
/// enabled, and warns if one is enabled for a newer version than the library.
fn declare_api(detected: Result<Detected, String>) {
    let newest = crate::newest_major();
    match (crate::feature_major(), &detected) {
        (Some(feature), Ok(detected)) if feature > detected.major => println!(
            "cargo:warning=the `clang_{feature}_0` feature declares the libclang {feature} API, \
             but the linked libclang is version {}; calling functions it lacks fails to link",
            detected.version
        ),
        (Some(_), _) => {}
        (None, Ok(detected)) => {
            let oldest = crate::oldest_major();
            if detected.major < oldest {
                println!(
                    "cargo:warning=the linked libclang is version {}, older than the oldest \
                     supported version; declaring the libclang {oldest} API",
                    detected.version
                );
            }
            let declared = detected.major.clamp(oldest, newest);
            for (major, _) in crate::VERSION_FEATURES {
                if major <= declared {
                    println!("cargo:rustc-cfg=feature=\"clang_{major}_0\"");
                }
            }
            // For the tests.
            println!(
                "cargo:rustc-env=CLANG_RS_SYS_DETECTED_LLVM_VERSION={}",
                detected.version
            );
        }
        (None, Err(reason)) => println!(
            "cargo:warning=cannot determine the version of the linked libclang ({reason}), so \
             the API of the newest supported version, libclang {newest}, is declared; enable \
             the `clang_<major>_0` feature of its version instead"
        ),
    }
    if let Ok(detected) = &detected {
        // DEP_CLANG_RS_LLVM_VERSION, as with `vendored`.
        println!("cargo:llvm_version={}", detected.version);
    }
}
