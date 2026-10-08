# clang-rs

Rust bindings to **libclang** that can build libclang — and everything it
depends on — from source and link it **statically** for the target being
compiled, following the model of
[rust-openssl](https://github.com/rust-openssl/rust-openssl)'s `vendored`
feature.

| Crate | Purpose | rust-openssl analogue |
|---|---|---|
| [`clang-rs-src`](clang-rs-src) | Builds libffi, ncurses and LLVM/Clang for the Cargo target and reports the static libraries, headers and link flags | `openssl-src` |
| [`clang-rs-sys`](clang-rs-sys) | Raw bindings to the libclang C API; links the system libclang, or with the `vendored` feature uses `clang-rs-src` | `openssl-sys` |

```toml
[dependencies]
clang-rs-sys = { version = "0.1", features = ["vendored"] }
```

The `clang_<major>_0` features (`clang_14_0` … `clang_23_0`) work like
[clang-sys](https://crates.io/crates/clang-sys)'s: they declare the libclang
API of that version, and with `vendored` they also select the LLVM release
that is built. Without any of them, the API of the libclang that is linked is
declared: with `vendored` the newest release, which is then built, and
otherwise the version of the system libclang, which the build script detects
(see [LLVM versions](#llvm-versions)).

With `vendored`, the final binary contains libclang, the Clang and LLVM
libraries, libffi and ncurses. It needs no LLVM, libffi or ncurses library at
run time, and none from the build machine are ever linked: the only dynamic
dependencies left are the platform's C and C++ runtimes (and on musl targets
not even those).

## How the vendored build works

`clang-rs-sys`'s build script calls `clang_rs_src::Build::new().build()`, which,
for Cargo's `TARGET`:

1. downloads **libffi 3.8.0** and **ncurses 6.6**, verifies their SHA-256
   checksums, and builds them as static, position-independent libraries with
   the target's C compiler (libffi is compiled directly with `cl.exe` for MSVC
   targets; ncurses does not support MSVC and is skipped there);
2. downloads the selected `llvm-project-<version>.src.tar.xz`, verifies its
   SHA-256, and unpacks only the parts needed to build libclang;
3. when cross compiling, builds LLVM's and Clang's TableGen tools for the
   host;
4. configures **LLVM/Clang** with CMake for the target — LLVM's
   `FindFFI` results are preset to the libffi from step 1, and all other
   optional dependencies (zlib, zstd, libxml2, libedit, ...) are disabled so
   nothing is picked up from the build machine — and builds the static
   libclang;
5. derives the exact set of static libraries libclang needs, and their link
   order, from LLVM's CMake export files, and installs them with the headers
   and Clang's builtin headers into `OUT_DIR`.

### About libffi and ncurses

Both are always built and linked, and LLVM is configured to use the libffi
built here. Note, however, what current LLVM actually uses them for:

* **ncurses (terminfo):** LLVM 19 removed LLVM's terminfo dependency
  (`LLVM_ENABLE_TERMINFO` no longer exists). For LLVM 14–18 (`clang_14_0` …
  `clang_18_0`), `clang-rs-src` points LLVM's terminfo support at the ncurses
  it built, and libclang links its `setupterm` & co.
* **libffi:** only LLVM's ExecutionEngine interpreter calls into libffi, and
  libclang does not contain the interpreter.

The `libffi.so`/`libtinfo.so` dependencies of distribution libclang packages
come from their monolithic `libLLVM.so`, which contains every LLVM component.
A static libclang links only what it uses, so with LLVM 19 and newer the
linker discards the (linked, but unreferenced) libffi and ncurses objects.

## LLVM versions

| Feature | Release built with `vendored` |
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
| `clang_23_0`, or none | 23.1.3 |

The features are cumulative, so the highest one enabled anywhere in the
dependency graph selects the release. libclang 14 is the oldest supported
version. Functions, types and constants added to the C API after it are only
declared with the feature of the version that introduced them, and constants
whose value changed have the value of the selected version. Without a version
feature, the declared API matches the libclang that is linked: the newest
release with `vendored`, or the system libclang's version, which the build
script detects from `llvm-config`, the library's file name or its resource
directory. See the
[`clang-rs-sys` README](clang-rs-sys/README.md#libclang-versions).

All ten releases were built and tested on x86_64 Linux, with their own
feature and (for 23.1.3) without one. LLVM 14 was also cross-built and tested
for `aarch64-unknown-linux-musl`, and LLVM 18 for `s390x-unknown-linux-gnu`.

## Supported targets

| Target | Built on | Verified |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Linux | built and tested natively |
| `x86_64-unknown-linux-musl` | Linux, with `cross` | built and tested |
| `aarch64-unknown-linux-gnu` | Linux, with `cross` | built and tested (QEMU) |
| `aarch64-unknown-linux-musl` | Linux, with `cross` | built and tested (QEMU) |
| `armv7-unknown-linux-musleabi` | Linux, with `cross` | built and tested (QEMU) |
| `i686-unknown-linux-musl` | Linux, with `cross` | built and tested |
| `loongarch64-unknown-linux-musl` | Linux, with `cross` | built and tested (QEMU) |
| `riscv64gc-unknown-linux-musl` | Linux, with `cross` | built and tested (QEMU) |
| `s390x-unknown-linux-gnu` | Linux, with `cross` | built and tested (QEMU) |
| `s390x-unknown-linux-musl` | Linux, with `cross +nightly` (tier 3: `-Zbuild-std`, image from `docker/`) | built and tested (QEMU) |
| `aarch64-apple-darwin` | macOS | CI configured, not yet run |
| `x86_64-apple-darwin` | macOS (also cross from Apple Silicon) | CI configured, not yet run |
| `x86_64-pc-windows-msvc` | Windows | CI configured, not yet run |
| `aarch64-pc-windows-msvc` | Windows (native or cross from x64) | CI configured, not yet run |

The Linux targets were verified with LLVM 23.1.3, using `cross test` on an
x86_64 Linux host. The macOS and Windows code paths (Visual Studio generator,
libffi compiled with `cl.exe`/`ml64`/`armasm64`, Apple cross-architecture
builds) could not be exercised there and are covered by the CI workflow
instead.

Any other target works as long as a C/C++ toolchain for it is configured the
way the [`cc`](https://crates.io/crates/cc) crate expects (`CC_<target>`,
`CXX_<target>`, ...).

`Cross.toml` configures [`cross`](https://github.com/cross-rs/cross) for all
Linux targets (`s390x-unknown-linux-musl` needs `cross` installed from git and
`cross +nightly`):

```sh
cross test -p clang-rs-sys --features vendored --target aarch64-unknown-linux-musl
```

`.github/workflows/ci.yml` builds and tests every target in the table on
GitHub-hosted runners.

## Requirements

* Rust 1.85 or newer for `vendored` builds (1.74 otherwise).
* CMake ≥ 3.20 and Python ≥ 3.8.
* A C++17 toolchain for the target, and one for the host when cross
  compiling. For musl targets the toolchain must provide a musl
  `libstdc++.a`, which gets linked into the binary.
* `sh` and `make` for non-MSVC targets; Ninja is used when available.
* For MSVC targets: Visual Studio 2019 16.8+ or 2022 with the C++ tools (and
  ARM64 tools for `aarch64-pc-windows-msvc`).

## Build time, disk space and caching

A vendored build of LLVM 23 compiles about 2,900 C++ files (older releases
somewhat fewer): roughly 3½ CPU-hours with the default single LLVM backend.
That is 4½ minutes on a 64-core machine, so an estimated hour on a 4-core CI
runner. It needs about 1.5 GB of disk space.

Set `CLANG_RS_SRC_CACHE_DIR` to a persistent directory to keep the download,
the unpacked sources and every finished build there. Any later build with
the same configuration — another profile, another project, a CI run that
restores the directory — reuses the finished libraries in seconds.

See the [`clang-rs-src` README](clang-rs-src/README.md) for all configuration
variables.

## Using libclang from a static build

libclang needs Clang's builtin headers (`stddef.h`, `stdarg.h`, the
intrinsics headers, ...) at run time, because it preprocesses code as a
compiler does and these headers come with the compiler, not the C library.
Pass their location with `-resource-dir=<dir>` when parsing. libclang doesn't
look next to itself. Without the option, it derives the location from the
compiler in `argv[0]`, as `<its directory>/../lib/clang/<major>`.
`clang_parseTranslationUnit` uses plain `clang` there, which makes it
`lib/clang/<major>` under the current directory. Programs that run on the build
machine can use `clang_rs_sys::VENDORED_RESOURCE_DIR`. Programs that are
distributed can call `clang_rs_sys::install_builtin_headers(dir)`, which
embeds the headers (compressed) and writes them to a directory of the
program's choosing on the machine it runs on. Build scripts of dependent crates get the
directory as `DEP_CLANG_RS_RESOURCE_DIR`.

libclang's default target is the Cargo target it was built for. A static
musl binary that parses code on glibc systems should pass
`--target=<gnu triple>` to libclang (or change the default, see the
[`clang-rs-src` README](clang-rs-src/README.md#default-target-triple)).

## Repository layout

```
clang-rs-src/       the -src crate (download and build logic)
clang-rs-sys/       the -sys crate (bindings, build script, tests, example)
docker/             cross image for s390x-unknown-linux-musl
Cross.toml          cross configuration for the Linux targets
.github/workflows/  CI for all supported targets and LLVM versions
```

## Updating the pinned versions

* LLVM: see [Adding an LLVM release](clang-rs-src/README.md#adding-an-llvm-release).
* libffi/ncurses: update `LIBFFI`/`NCURSES` (version, file name, SHA-256,
  URLs) in `clang-rs-src/src/source.rs`.

## License

The code in this repository is licensed under either of Apache-2.0
([LICENSE-APACHE](LICENSE-APACHE)) or MIT ([LICENSE-MIT](LICENSE-MIT)) at
your option. LLVM is licensed under Apache-2.0 with LLVM exceptions; libffi
and ncurses under their own MIT-style licenses.
