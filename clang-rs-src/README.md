# clang-rs-src

Builds **libclang** (and every Clang and LLVM library it depends on), **libffi**
and **ncurses** from source as static libraries, for whatever target Cargo is
compiling for. It is meant to be used from the build script of a `-sys` crate,
the same way [`openssl-src`](https://crates.io/crates/openssl-src) is used by
`openssl-sys` when its `vendored` feature is enabled. See
[`clang-rs-sys`](../clang-rs-sys) for a complete example.

```rust
// build.rs
fn main() {
    let artifacts = clang_rs_src::Build::new().build();
    artifacts.print_cargo_metadata();
}
```

## Selecting the LLVM release

The `clang_<major>_0` features select the LLVM/Clang release that is built.
They follow [clang-sys](https://crates.io/crates/clang-sys)'s naming and are
cumulative in the same way (each one implies the features of older versions),
so the **highest enabled feature wins**. Without any of them the newest
release is built. `Build::llvm_version(major)` overrides the features.

| Feature | Release built |
|---|---|
| `clang_14_0` | 14.0.6 |
| `clang_15_0` | 15.0.7 |
| `clang_16_0` | 16.0.6 |
| `clang_17_0` | 17.0.6 |
| `clang_18_0` | 18.1.8 |
| `clang_19_0` | 19.1.7 |
| `clang_20_0` | 20.1.8 |
| `clang_21_0` | 21.1.8 |
| `clang_22_0` | 22.1.8 |
| `clang_23_0` | 23.1.3 (also built when none of these features is enabled) |

Each is the newest release of its major version. Other sources can be built
with `CLANG_RS_SRC_LLVM_TARBALL` or `CLANG_RS_SRC_LLVM_SOURCE_DIR`; if a
version feature is enabled as well, the sources must be at least that version.

## Downloads

No sources are bundled with the crate. These release tarballs are downloaded
at build time with the system's `curl` (or `wget`, or PowerShell on Windows) and checked
against pinned SHA-256 checksums. TLS certificates are verified against the
operating system's trust store (so corporate CAs work), and the system's proxy
settings, such as `HTTPS_PROXY`, are honored.

| Component | Version | Downloaded from |
|---|---|---|
| LLVM/Clang | see above | `https://github.com/llvm/llvm-project/releases/download/llvmorg-<version>/llvm-project-<version>.src.tar.xz` |
| libffi | 3.8.0 | `https://github.com/libffi/libffi/releases/download/v3.8.0/libffi-3.8.0.tar.gz` |
| ncurses | 6.6 | `https://invisible-island.net/archives/ncurses/ncurses-6.6.tar.gz`, or the GNU mirror |

They are kept in a download directory (`CLANG_RS_SRC_DOWNLOAD_DIR`, by default
`src/downloads` in the cache or build directory) and are not downloaded again
while a file with the right checksum is there. For offline builds, put the
three files in a directory and point `CLANG_RS_SRC_DOWNLOAD_DIR` at it;
`CLANG_RS_SRC_OFFLINE=1` turns any attempt to download into an error.

## What the build does

1. Builds libffi and ncurses (wide-character `libncursesw`) as static,
   position-independent libraries with the target's C compiler. For MSVC
   targets libffi is compiled directly with `cl.exe` (its autotools build
   needs a Unix shell there) and ncurses is skipped: it does not support MSVC,
   and LLVM never uses terminfo on Windows.
2. When cross compiling, builds LLVM's and Clang's TableGen tools for the
   host.
3. Configures LLVM + Clang with CMake for the target (`LLVM_HOST_TRIPLE` and
   `LLVM_DEFAULT_TARGET_TRIPLE` are set to the target) and builds the static
   libclang. LLVM is linked against the libffi from step 1 by setting the
   result variables of its `FindFFI` module, so no system libffi can be found.
   LLVM 14–18, which still have a terminfo dependency, are pointed at the
   ncurses from step 1 the same way. All other optional dependencies — zlib,
   zstd, libxml2, libedit, libpfm, curl, httplib, Z3, ICU, iconv — are
   disabled.
4. Reads the CMake export files to compute exactly which static libraries
   libclang needs, in an order that also works with single-pass linkers, plus
   the system libraries they require (`rt`, `dl`, `m`, `psapi`, ...), and
   installs them with the headers and Clang's builtin headers into one
   directory.

`Artifacts::print_cargo_metadata` then emits `rustc-link-lib=static=...` for
libclang, the Clang and LLVM libraries, libffi and ncurses, plus the C++
standard library: `libstdc++` dynamically for glibc, `libstdc++.a` from the
cross compiler for musl, `libc++` on Apple platforms, and nothing for MSVC
(which selects its runtime through `/DEFAULTLIB`).

> **Note:** LLVM 19 removed LLVM's terminfo dependency, and libffi is only used
> by LLVM's ExecutionEngine interpreter, which libclang does not contain. So
> with LLVM 14–18, libclang links ncurses' terminfo functions (`setupterm` &
> co.), while with LLVM 19 and newer it references neither library. They are
> still always built and linked, so that anything in the final binary that
> needs them resolves to these copies rather than to libraries from the build
> machine.

## Requirements

* CMake ≥ 3.20 and Python ≥ 3.8.
* Rust 1.85 or newer.
* A C/C++17 toolchain for the target, plus one for the host when cross
  compiling. Compilers are found exactly as the [`cc`](https://crates.io/crates/cc)
  crate finds them (`CC_<target>`, `CXX_<target>`, `AR_<target>`,
  `CFLAGS_<target>`, `CROSS_COMPILE`, ...). For targets `cc` has no default
  for (`armv7-unknown-linux-musleabi`, `loongarch64-unknown-linux-musl`,
  `s390x-unknown-linux-musl`, ...) a `<arch>-linux-musl*-gcc` toolchain on
  `PATH` is used.
* `sh` and `make` for all non-MSVC targets.
* [Ninja](https://ninja-build.org) is used automatically when found and is
  strongly recommended (except for MSVC, see below).
* Visual Studio 2019 16.8+ (or 2022) with the C++ tools — and the ARM64 tools
  for `aarch64-pc-windows-msvc` — for MSVC targets. MSVC builds use CMake's
  Visual Studio generator by default, which needs no Developer Command Prompt;
  `CMAKE_GENERATOR=Ninja` works too when run from one.

A build compiles 2,000–3,000 C++ files (about 3½ CPU-hours for LLVM 23 with
the default single LLVM backend) and needs about 1.5 GB of disk space.

## Configuration

| Variable | Effect |
|---|---|
| `CLANG_RS_SRC_DOWNLOAD_DIR` | Where tarballs are downloaded to, and looked for first. |
| `CLANG_RS_SRC_OFFLINE` | `1`: fail instead of downloading. |
| `CLANG_RS_SRC_LLVM_URL`, `CLANG_RS_SRC_LIBFFI_URL`, `CLANG_RS_SRC_NCURSES_URL` | Download the pinned tarball from this URL instead (e.g. a mirror); its checksum is still verified. |
| `CLANG_RS_SRC_LLVM_SOURCE_DIR` | Build this unpacked `llvm-project` tree instead of a pinned release. |
| `CLANG_RS_SRC_LLVM_TARBALL` | Build this `llvm-project-*.src.tar.xz` instead of a pinned release. |
| `CLANG_RS_SRC_LLVM_TARGETS` | `LLVM_TARGETS_TO_BUILD`. Default: the target's architecture (`X86`, `AArch64`, `ARM`, `LoongArch`, `RISCV`, `SystemZ`, ...); `all` builds every backend. |
| `CLANG_RS_SRC_CACHE_DIR` | Keep downloads, unpacked sources and finished builds here and reuse them across profiles and projects. |
| `CLANG_RS_SRC_BUILD_DIR` | Unpack and build here instead of in `OUT_DIR`; builds of the same configuration (e.g. debug and release) share it, so they must not run concurrently. |
| `CLANG_RS_SRC_CMAKE_ARGS` | Extra whitespace-separated `-DKEY=VALUE` arguments for the LLVM configuration. |
| `CLANG_RS_SRC_STATIC_CXX_STDLIB` | `1`: link `libstdc++.a` on glibc targets too. |
| `CMAKE_TOOLCHAIN_FILE_<target>`, `CMAKE_GENERATOR`, `CMAKE` | As for the `cmake` crate. |

On Windows hosts, sources and build trees default to `%TEMP%\clang-rs-src`
instead of `OUT_DIR`: object files end up ~165 characters below LLVM's build
directory, which would exceed `MAX_PATH` under a typical `target` directory.
The installed artifacts are always placed in `OUT_DIR`.

The same settings are available programmatically on `Build`. The
`all-targets` feature builds every LLVM backend.

Clang can parse code for any target with any backend selection; backends are
only needed for code generation and for validating inline assembly, so the
default keeps build times down.

## Default target triple

libclang parses for `LLVM_DEFAULT_TARGET_TRIPLE` unless told otherwise, and
this crate sets it to the Cargo target (e.g. `x86_64-unknown-linux-musl`),
as a native Clang for that platform would. That choice affects header search:
for musl triples Clang expects a musl system, and it won't find a GCC/libstdc++
installation made for glibc. A static musl binary that parses code on glibc
systems should pass `--target=<triple>` to libclang, or change the default
with `CLANG_RS_SRC_CMAKE_ARGS=-DLLVM_DEFAULT_TARGET_TRIPLE=x86_64-unknown-linux-gnu`.

## The resource directory

libclang needs Clang's builtin headers (`stddef.h`, `stdarg.h`, ...) at run
time, from Clang's resource directory (`lib/clang/<major>`, or
`lib/clang/<version>` for LLVM 14 and 15). Pass it as `-resource-dir=<dir>`
when parsing. libclang doesn't look next to itself. Without the option, it
derives the directory from the compiler in `argv[0]`, as
`<its directory>/../lib/clang/<major>`. `clang_parseTranslationUnit` uses
plain `clang` there, which makes it relative to the current directory.
`Artifacts::resource_dir()` (also exported as `DEP_<links>_RESOURCE_DIR`)
points at the built copy; ship it with your program.

## Adding an LLVM release

1. Add it to `LLVM_RELEASES` in `src/source.rs` (version and the SHA-256 of
   `llvm-project-<version>.src.tar.xz`).
2. Add the `clang_<major>_0` feature here, to `feature_llvm_major` in
   `src/lib.rs`, and to `clang-rs-sys`: its `Cargo.toml`,
   `build/find_vendored.rs` and `tests/parse.rs` (the `version_features` test
   checks all of them).
3. Regenerate the bindings of `clang-rs-sys` with
   `clang-rs-sys/scripts/generate-bindings.py`. It fetches the headers of
   every supported release, and the new release becomes the API that
   `clang-rs-sys` declares without a version feature.

## License

The code of this crate is licensed under either of Apache-2.0 or MIT at your
option. The software it builds is not: LLVM is licensed under Apache-2.0
with LLVM exceptions, and libffi and ncurses under their own permissive
MIT-style licenses (included in their source tarballs).
