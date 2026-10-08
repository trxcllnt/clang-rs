//! Raw FFI bindings to [libclang], the stable C interface to Clang.
//!
//! By default the system's libclang is linked dynamically (see the README for
//! how it is located). With the **`vendored`** feature, libclang and every
//! library it depends on — LLVM, libffi and ncurses — are built from source
//! for the Cargo target by [`clang-rs-src`] and linked statically, in the same
//! way `openssl-sys` uses `openssl-src`.
//!
//! # libclang versions
//!
//! Like `clang-sys`, the crate has a feature for every supported libclang
//! version, `clang_14_0` through `clang_23_0`, which selects the API that is
//! declared. The features are cumulative, so enabling `clang_17_0` also
//! enables `clang_14_0` through `clang_16_0`, and the highest one counts:
//!
//! * Functions, types and constants introduced after libclang 14 are only
//!   declared from the version that introduced them on, and the few that were
//!   removed only up to the last version that has them.
//! * Constants whose value changed between versions, such as
//!   [`CXCursor_TranslationUnit`] or [`CINDEX_VERSION_MINOR`], have the value
//!   of the selected version.
//!
//! The documentation of each such item says which versions have it. With
//! `vendored`, the selected version is also the LLVM release that is built.
//!
//! Without any version feature, the API of the libclang that is linked is
//! declared:
//!
//! * with `vendored`, that of the newest supported release, libclang 23,
//!   which is then the one built;
//! * otherwise that of the system libclang's version, which the build script
//!   detects (from `llvm-config`, the library's file name or its resource
//!   directory). It sets the `feature` cfgs of that version for this crate,
//!   though Cargo and dependent crates don't see them as enabled features. If
//!   the version can't be determined, the build script warns and the API of
//!   the newest version is declared.
//!
//! # The resource directory
//!
//! libclang preprocesses the code it parses as a compiler does, so every
//! `#include` has to be found. Some standard headers come with the compiler
//! rather than the C library: `stddef.h`, `stdarg.h`, `stdint.h`,
//! `stdbool.h`, `limits.h`, `float.h`, the intrinsics headers (`immintrin.h`,
//! `arm_neon.h`, ...) and a few more. The C library's own headers include
//! them, so almost any real code needs them. They are written in terms of
//! Clang's builtins, so they belong to one Clang version, and they are not
//! part of the library. libclang looks for them at run time in Clang's
//! resource directory, which you should pass as `-resource-dir=<dir>` to
//! [`clang_parseTranslationUnit`]. libclang doesn't look next to itself.
//! Without the option, it derives the directory from the compiler in
//! `argv[0]`, as `<its directory>/../lib/clang/<major>`.
//! `clang_parseTranslationUnit` uses plain `clang` there, which makes it
//! `lib/clang/<major>` under the current directory.
//!
//! The vendored build exports their location as [`VENDORED_RESOURCE_DIR`]
//! (useful for tests and tools run on the build machine) and to dependent
//! build scripts as `DEP_CLANG_RS_RESOURCE_DIR`. Programs that are
//! distributed can install them on the machine they run on with
//! `install_builtin_headers`, which embeds them compressed.
//!
//! [libclang]: https://clang.llvm.org/docs/LibClang.html
//! [`clang-rs-src`]: https://crates.io/crates/clang-rs-src

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

/// The C `time_t` of the target: 64 bits everywhere except on 32-bit glibc
/// targets (musl has used a 64-bit `time_t` since 1.2.0, and MSVC defaults to
/// `__time64_t`).
#[cfg(all(target_os = "linux", target_env = "gnu", target_pointer_width = "32"))]
pub type time_t = std::os::raw::c_long;
/// The C `time_t` of the target: 64 bits everywhere except on 32-bit glibc
/// targets (musl has used a 64-bit `time_t` since 1.2.0, and MSVC defaults to
/// `__time64_t`).
#[cfg(not(all(target_os = "linux", target_env = "gnu", target_pointer_width = "32")))]
pub type time_t = i64;

mod bindings {
    #![allow(
        clippy::all,
        clippy::pedantic,
        missing_docs,
        non_camel_case_types,
        non_snake_case,
        non_upper_case_globals,
        // bindgen allows lints that older compilers don't know about.
        unknown_lints,
        rustdoc::broken_intra_doc_links,
        rustdoc::invalid_html_tags,
        rustdoc::bare_urls
    )]
    use super::time_t;
    include!("bindings.rs");
}
pub use bindings::*;

/// Path of Clang's resource directory produced by the vendored build, or
/// `None` without the `vendored` feature.
///
/// This is a path on the machine that built the crate, so it is only valid
/// for programs that run there (tests, build tools). Pass it to libclang as
/// `-resource-dir=<path>`.
pub const VENDORED_RESOURCE_DIR: Option<&str> = option_env!("CLANG_RS_SYS_RESOURCE_DIR");

/// The LLVM version libclang was built from, or `None` without the
/// `vendored` feature.
pub const VENDORED_LLVM_VERSION: Option<&str> = option_env!("CLANG_RS_SYS_LLVM_VERSION");

/// Clang's builtin headers of the vendored libclang, compressed. There are
/// none unless a vendored libclang was built.
#[cfg(any(feature = "vendored", docsrs))]
mod builtin_headers {
    // docs.rs documents the crate without `vendored`, and so without headers.
    #![cfg_attr(not(feature = "vendored"), allow(dead_code, unused_imports))]

    use std::fs;
    use std::io::{self, Read};
    use std::path::Path;

    include!(concat!(env!("OUT_DIR"), "/builtin_headers.rs"));

    #[cfg(feature = "vendored")]
    pub(crate) fn install(resource_dir: &Path) -> io::Result<()> {
        let mut zlib = flate2::read::ZlibDecoder::new(ZLIB);
        let mut contents = Vec::new();
        for &(name, size) in FILES {
            contents.resize(size, 0);
            zlib.read_exact(&mut contents)?;
            let path = resource_dir.join(name);
            if fs::read(&path).is_ok_and(|existing| existing == contents) {
                continue;
            }
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&path, &contents)?;
        }
        // Reading the end of the stream also verifies its checksum.
        if zlib.read(&mut [0])? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the embedded builtin headers are longer than expected",
            ));
        }
        Ok(())
    }

    #[cfg(not(feature = "vendored"))]
    pub(crate) fn install(_: &Path) -> io::Result<()> {
        unreachable!("no builtin headers are embedded")
    }
}

/// Installs Clang's builtin headers of the vendored libclang into
/// `resource_dir`, making it a Clang resource directory: the headers end up
/// in `resource_dir/include`.
///
/// Programs that are distributed can't use [`VENDORED_RESOURCE_DIR`], a path
/// on the build machine. This installs the headers where the program chooses
/// instead, such as in a directory named after [`VENDORED_LLVM_VERSION`] in
/// its data directory. To use them, pass `-resource-dir=<resource_dir>` to
/// libclang when parsing ([`clang_parseTranslationUnit`] and the like).
///
/// The headers are embedded, compressed with zlib, in programs that call
/// this function, which adds 0.5–0.7 MB depending on the LLVM version, and
/// are decompressed with `flate2` while they are installed. Files that
/// already have the right contents aren't written again, so it is cheap to
/// call at every start. Other files in the directory are left alone.
///
/// Only available with the `vendored` feature.
///
/// # Errors
///
/// Fails if a directory or file can't be written, and with
/// [`std::io::ErrorKind::Unsupported`] if `CLANG_RS_SYS_NO_VENDOR` made the
/// build link a system libclang, which comes with its own headers.
#[cfg(any(feature = "vendored", docsrs))]
pub fn install_builtin_headers<P: AsRef<std::path::Path>>(resource_dir: P) -> std::io::Result<()> {
    if builtin_headers::FILES.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "clang-rs-sys links a system libclang (CLANG_RS_SYS_NO_VENDOR is set), which \
             comes with its own builtin headers",
        ));
    }
    builtin_headers::install(resource_dir.as_ref())
}
