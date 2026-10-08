use std::env;
use std::fs;
use std::path::PathBuf;

mod find_normal;
#[cfg(feature = "vendored")]
mod find_vendored;

/// The `clang_<major>_0` features, newest first, and whether each one is
/// enabled. The newest one is the version `src/bindings.rs` is generated
/// from, whose API is declared when no version feature is enabled (unless a
/// system libclang of another version is linked, see `find_normal`).
const VERSION_FEATURES: [(u32, bool); 10] = [
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
];

/// The version selected by the version features: the highest enabled one, as
/// they are cumulative.
fn feature_major() -> Option<u32> {
    VERSION_FEATURES
        .iter()
        .find_map(|&(major, enabled)| enabled.then_some(major))
}

fn newest_major() -> u32 {
    VERSION_FEATURES[0].0
}

fn oldest_major() -> u32 {
    VERSION_FEATURES[VERSION_FEATURES.len() - 1].0
}

/// Writes the builtin headers that `install_builtin_headers` embeds:
/// `$OUT_DIR/builtin_headers.zlib` holds their contents, concatenated and
/// compressed with zlib, and `$OUT_DIR/builtin_headers.rs` lists their paths
/// relative to the resource directory and their sizes.
fn write_builtin_headers(files: &[(String, usize)], zlib: &[u8]) {
    let mut code = String::from(
        "/// Clang's builtin headers: their paths relative to the resource directory\n\
         /// and their sizes, in the order in which `ZLIB` contains them.\n\
         pub(crate) static FILES: &[(&str, usize)] = &[\n",
    );
    for (name, size) in files {
        code += &format!("    ({name:?}, {size}),\n");
    }
    code += "];\n\n\
             /// The contents of `FILES`, concatenated and compressed with zlib.\n\
             pub(crate) static ZLIB: &[u8] =\n    \
             include_bytes!(concat!(env!(\"OUT_DIR\"), \"/builtin_headers.zlib\"));\n";
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    fs::write(out_dir.join("builtin_headers.zlib"), zlib)
        .expect("cannot write builtin_headers.zlib");
    fs::write(out_dir.join("builtin_headers.rs"), code).expect("cannot write builtin_headers.rs");
}

fn main() {
    println!("cargo:rerun-if-changed=build");
    for var in [
        "CLANG_RS_SYS_NO_VENDOR",
        "CLANG_RS_SYS_LIB_DIR",
        "LIBCLANG_PATH",
        "LLVM_CONFIG_PATH",
        // Where the C compiler looks for libraries (see `find_normal`).
        "LIBRARY_PATH",
        "DOCS_RS",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    // Only a vendored build has headers to embed.
    write_builtin_headers(&[], &[]);

    // docs.rs has neither the time nor the network access to build LLVM, and
    // documentation doesn't need to link.
    if env::var_os("DOCS_RS").is_some() {
        return;
    }

    #[cfg(feature = "vendored")]
    {
        let no_vendor = env::var("CLANG_RS_SYS_NO_VENDOR").is_ok_and(|v| v != "0");
        if !no_vendor {
            find_vendored::link();
            return;
        }
    }

    find_normal::link();
}
