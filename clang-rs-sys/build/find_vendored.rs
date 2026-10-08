use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::ZlibEncoder;
use flate2::Compression;

/// Builds libclang and its dependencies from source and links them
/// statically.
pub fn link() {
    // The release whose API is declared. It is passed to clang-rs-src
    // explicitly, rather than relying on the features forwarded to it, so that
    // the release that is built matches the declarations even when other
    // crates enable clang-rs-src's features too.
    let major = crate::feature_major().unwrap_or_else(crate::newest_major);
    let artifacts = clang_rs_src::Build::new().llvm_version(major).build();
    artifacts.print_cargo_metadata();

    // Exposed to the crate as `VENDORED_RESOURCE_DIR` / `VENDORED_LLVM_VERSION`.
    println!(
        "cargo:rustc-env=CLANG_RS_SYS_RESOURCE_DIR={}",
        artifacts.resource_dir().display()
    );
    println!(
        "cargo:rustc-env=CLANG_RS_SYS_LLVM_VERSION={}",
        artifacts.llvm_version()
    );
    println!("cargo:vendored=1");

    // Embedded for `install_builtin_headers`, concatenated and compressed.
    let mut headers = Vec::new();
    find_files(artifacts.resource_dir(), "", &mut headers);
    headers.sort();
    let mut zlib = ZlibEncoder::new(Vec::new(), Compression::best());
    let mut files = Vec::new();
    for (name, path) in headers {
        let contents =
            fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        zlib.write_all(&contents)
            .expect("compressing in memory failed");
        files.push((name, contents.len()));
    }
    let zlib = zlib.finish().expect("compressing in memory failed");
    crate::write_builtin_headers(&files, &zlib);
}

/// The files below `dir` (whose path relative to the top directory is
/// `prefix`), with their paths relative to the top directory.
fn find_files(dir: &Path, prefix: &str, files: &mut Vec<(String, PathBuf)>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
        let path = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .expect("the name of a builtin header is not UTF-8");
        let name = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            find_files(&path, &name, files);
        } else {
            files.push((name, path));
        }
    }
}
