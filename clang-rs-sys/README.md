# clang-rs-sys

Raw FFI bindings to [libclang](https://clang.llvm.org/docs/LibClang.html),
Clang's stable C API.

* **Default:** links the libclang already installed on the system
  dynamically.
* **`vendored` feature:** builds libclang, the LLVM and Clang libraries it is
  made of, libffi and ncurses from source for the Cargo target with
  [`clang-rs-src`](../clang-rs-src), and links all of them **statically**, exactly
  like `openssl-sys`'s `vendored` feature does with `openssl-src`. The
  resulting binaries do not depend on any LLVM, libffi or ncurses library on
  the machine they run on.

```toml
[dependencies]
clang-rs-sys = { version = "0.1", features = ["vendored"] }
```

```rust
use clang_rs_sys::*;
use std::ffi::CStr;

fn main() {
    unsafe {
        let version = clang_getClangVersion();
        println!("{}", CStr::from_ptr(clang_getCString(version)).to_string_lossy());
        clang_disposeString(version);
    }
}
```

Rust 1.74 or newer is required, 1.85 with the `vendored` feature.

`examples/dump_ast.rs` is a slightly larger program:
`cargo run --example dump_ast --features vendored -- some/file.c`.

## Features

| Feature | Effect |
|---|---|
| `vendored` | Build libclang and its dependencies from source and link them statically. |
| `all-targets` | With `vendored`: build every LLVM backend, not just the target's. |
| `clang_14_0` … `clang_23_0` | Declare the API of that libclang version; with `vendored`, also build that LLVM release. Without any of them: the version of the libclang that is linked. |

### libclang versions

The version features work like those of
[clang-sys](https://crates.io/crates/clang-sys): they select the libclang
version whose API the crate declares. libclang 14 is the oldest supported
version. Functions, types and constants added to the C API since then are
only declared with the feature of the version that introduced them; for
example, `clang_createIndexWithOptions` and `CXIndexOptions` need
`clang_17_0`. Each feature implies the features of older versions, so enable
the one for the oldest libclang your code has to work with; dependencies that
enable different versions end up with the highest one.

**Without any version feature, the API of the libclang that is linked is
declared.** With `vendored`, that is the newest supported release, which is
then the one built. It moves forward when the crate adds support for a new
release, so enable a feature to pin it. With a system libclang, the build
script [detects its version](#detecting-the-version) and declares that
version's API.

| Feature | Added to the API by that version | Release built with `vendored` |
|---|---|---|
| `clang_14_0` | — (oldest supported version) | 14.0.6 |
| `clang_15_0` | 15 constants | 15.0.7 |
| `clang_16_0` | 9 functions, 2 types, 2 constants | 16.0.6 |
| `clang_17_0` | 9 functions, 8 types, 56 constants | 17.0.6 |
| `clang_18_0` | 2 constants | 18.1.8 |
| `clang_19_0` | 2 functions, 1 type, 43 constants | 19.1.7 |
| `clang_20_0` | 4 functions, 13 constants | 20.1.8 |
| `clang_21_0` | 11 functions, 17 constants | 21.1.8 |
| `clang_22_0` | 1 constant | 22.1.8 |
| `clang_23_0` (or none) | 2 functions, 1 type, 3 constants | 23.1.3 |

A few items differ between versions in other ways; with each feature, the
crate declares them as that version does:

* `CXCursor_TranslationUnit` is 300 in libclang 14 and 350 from 15 on.
  `CINDEX_VERSION_MINOR` and the range markers `CXCursor_LastExpr`,
  `CXCursor_LastStmt` and `CXCursor_LastExtraDecl` also change between
  versions.
* `CXCursor_OMPArraySectionExpr` became `CXCursor_ArraySectionExpr` in
  libclang 19, so it is only declared up to `clang_18_0`.
* `clang_visitChildrenWithBlock` and the other `*WithBlock` functions are
  exported by older versions too, but before libclang 17 the headers only
  declare them (and the types they use) for compilers that support blocks.
  They need `clang_17_0`.

The documentation of every guarded item says which versions have it.

With `vendored`, the selected version is also the release that is built.
Without `vendored`, the features only control what is declared. If one is
enabled for a newer version than the system libclang, the build script
warns: calling a function that the library lacks fails to link.

## Linking a system libclang (default)

The library is searched for in `CLANG_RS_SYS_LIB_DIR`, then `LIBCLANG_PATH`,
then the `--libdir` of `llvm-config` (or `LLVM_CONFIG_PATH`), then the
linker's default paths. On Linux this needs the unversioned `libclang.so`
symlink, which distributions ship in their `libclang-dev` packages.

Setting `CLANG_RS_SYS_NO_VENDOR=1` makes a `vendored` build use the system
library anyway (like `OPENSSL_NO_VENDOR`).

### Detecting the version

Without a version feature, the build script determines the version of the
libclang it links and declares that version's API. It does so by setting
the `feature` cfgs of the version's `clang_<major>_0` feature and all older
ones for this crate. A build script can't enable Cargo features, so Cargo
and dependent crates don't see them as enabled. Instead, dependent crates'
build scripts get the version as `DEP_CLANG_RS_LLVM_VERSION`. The build
script takes the version from the first of these that tells it:

1. `llvm-config --version`, if `llvm-config` supplied the library directory;
2. the name of the file that `libclang.so` points to, like
   `libclang-19.so.19` (Debian, Ubuntu) or `libclang.so.19.1.7`;
3. the resource directory next to the library, `lib/clang/<version>`, where
   libclang finds its builtin headers.

If no directory is configured, native builds on Linux and other Unix systems
(except macOS) look for the library in the C compiler's default search path
(`cc -print-file-name=libclang.so`). Xcode's libclang can't be used for
detection, because its version numbers are Apple's own. If the version can't
be determined, the build script warns and declares the API of the newest
supported version. A version feature you enable always takes precedence.

## Vendored builds

See the [`clang-rs-src` README](../clang-rs-src/README.md) for the requirements
(CMake ≥ 3.20, Python ≥ 3.8, a C++17 toolchain for the target and the host),
how compilers are selected, and all configuration variables. A first build
takes a while — roughly 3½ CPU-hours — so consider setting
`CLANG_RS_SRC_CACHE_DIR` to share finished builds between debug/release profiles,
projects and CI runs.

The following metadata is available to build scripts of crates that depend
on `clang-rs-sys` (`links = "clang_rs"`):

| Variable | Contents |
|---|---|
| `DEP_CLANG_RS_ROOT` | Installation directory |
| `DEP_CLANG_RS_INCLUDE` | Headers (`clang-c/`, `ffi.h`, `curses.h`, ...) |
| `DEP_CLANG_RS_LIB` | Static libraries |
| `DEP_CLANG_RS_RESOURCE_DIR` | Clang's resource directory (builtin headers) |
| `DEP_CLANG_RS_LLVM_VERSION` | LLVM version (also set for a system libclang whose version was detected, possibly only its major version) |
| `DEP_CLANG_RS_VENDORED` | `1` |

### Builtin headers

libclang preprocesses code as a compiler does, so it needs Clang's builtin
headers: `stddef.h`, `stdarg.h`, `stdint.h`, the intrinsics headers and
others that come with the compiler rather than the C library, and that the
C library's headers include. They are tied to the Clang version and are not
part of the library. libclang looks for them at run time in Clang's resource
directory, which you should pass as `-resource-dir=<dir>` to
`clang_parseTranslationUnit`. libclang doesn't look next to itself. Without
the option, it derives the directory from the compiler in `argv[0]`, as
`<its directory>/../lib/clang/<major>`. `clang_parseTranslationUnit` uses
plain `clang` there, which makes it `lib/clang/<major>` under the current
directory. With `clang_parseTranslationUnit2FullArgv` and `/usr/bin/gcc`, it
is `/usr/lib/clang/<major>`, which may hold another Clang installation's
headers.

Clang always uses its own builtin headers in place of GCC's, including when
`argv[0]` names a gcc: the headers it reports (`clang_getInclusions`) come
from the resource directory, plus the C library's from the system include
directories.

* Programs that run on the build machine (tests, code generators) can use
  the built copy at `clang_rs_sys::VENDORED_RESOURCE_DIR`.
* Programs that are distributed can call
  `clang_rs_sys::install_builtin_headers(dir)`, which embeds the headers
  compressed with zlib (0.5–0.7 MB, only in programs that call it) and
  writes them to `dir/include` on the machine the program runs on. Use a directory per version, for
  example one named after `VENDORED_LLVM_VERSION`. Files that are already
  up to date aren't rewritten, so it can run at every start.

```rust
let resource_dir = data_dir.join("clang").join(clang_rs_sys::VENDORED_LLVM_VERSION.unwrap());
clang_rs_sys::install_builtin_headers(&resource_dir)?;
let arg = CString::new(format!("-resource-dir={}", resource_dir.display()))?;
// ... pass `arg` to clang_parseTranslationUnit ...
```

Build scripts of dependent crates also get the directory as
`DEP_CLANG_RS_RESOURCE_DIR`.

## Bindings

`src/bindings.rs` is generated by `scripts/generate-bindings.py`. bindgen
turns the `clang-c` headers of every supported LLVM release into Rust. The
script takes the newest release's output and guards each item that isn't the
same in all releases, using the release that introduced it (functions must
also be in that release's list of exported symbols,
`clang/tools/libclang/libclang.map`). Items that changed or were removed get a
definition for each range of releases. Before writing the file, the script
checks that each feature setting declares exactly that release's API. It
also generates `tests/link.rs`, which takes the address of every declared
function so that linking fails if libclang lacks one. Enumerations are plain
integer constants (`CXCursor_StructDecl`, ...), named exactly as in C.

The bindings are target independent except for `time_t` (defined per target
in `src/lib.rs`) and `CXIndexOptions`, whose bit-fields MSVC lays out
differently from the Itanium ABI used everywhere else; the generation script
adds the MSVC padding behind `#[cfg(target_env = "msvc")]`. The tests check
that libclang accepts `size_of::<CXIndexOptions>()` on every target.

## License

Licensed under either of Apache-2.0 or MIT at your option. libclang itself is
licensed under Apache-2.0 with LLVM exceptions.
