//! Smoke tests that exercise libclang through the raw bindings.
//!
//! Only Clang's own builtin headers are used (`-ffreestanding -nostdlibinc`),
//! so the results don't depend on the system headers of the machine the tests
//! run on. That matters under `cross`, where `/usr/include` holds the glibc
//! headers of the build container regardless of the target, and clang
//! searches `/usr/include` before its builtin headers for musl targets.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_ulong};
use std::ptr;

use clang_rs_sys::*;

fn into_string(s: CXString) -> String {
    unsafe {
        let ptr = clang_getCString(s);
        let string = if ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        };
        clang_disposeString(s);
        string
    }
}

/// Top-level declarations of the main file as `(kind, name)` pairs, plus
/// the rendered error diagnostics.
fn parse(
    file_name: &str,
    source: &str,
    extra_args: &[&str],
) -> (Vec<(CXCursorKind, String)>, Vec<String>) {
    let mut args: Vec<CString> = ["-ffreestanding", "-nostdlibinc", "-Wall"]
        .iter()
        .chain(extra_args)
        .map(|a| CString::new(*a).unwrap())
        .collect();
    let has_resource_dir = extra_args.iter().any(|a| a.starts_with("-resource-dir"));
    if let Some(dir) = VENDORED_RESOURCE_DIR.filter(|_| !has_resource_dir) {
        args.push(CString::new(format!("-resource-dir={dir}")).unwrap());
    }
    let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    let name = CString::new(file_name).unwrap();
    let contents = CString::new(source).unwrap();
    let mut unsaved = CXUnsavedFile {
        Filename: name.as_ptr(),
        Contents: contents.as_ptr(),
        Length: source.len() as c_ulong,
    };

    extern "C" fn collect(cursor: CXCursor, _: CXCursor, out: CXClientData) -> CXChildVisitResult {
        unsafe {
            if clang_Location_isFromMainFile(clang_getCursorLocation(cursor)) != 0 {
                let out = &mut *(out as *mut Vec<(CXCursorKind, String)>);
                out.push((
                    clang_getCursorKind(cursor),
                    into_string(clang_getCursorSpelling(cursor)),
                ));
            }
        }
        CXChildVisit_Continue
    }

    unsafe {
        let index = clang_createIndex(0, 0);
        assert!(!index.is_null());
        let mut tu = ptr::null_mut();
        let err = clang_parseTranslationUnit2(
            index,
            name.as_ptr(),
            argv.as_ptr(),
            argv.len() as c_int,
            &mut unsaved,
            1,
            CXTranslationUnit_None,
            &mut tu,
        );
        assert_eq!(err, CXError_Success, "clang_parseTranslationUnit2 failed");

        let mut errors = Vec::new();
        for i in 0..clang_getNumDiagnostics(tu) {
            let diag = clang_getDiagnostic(tu, i);
            if clang_getDiagnosticSeverity(diag) >= CXDiagnostic_Error {
                errors.push(into_string(clang_formatDiagnostic(
                    diag,
                    clang_defaultDiagnosticDisplayOptions(),
                )));
            }
            clang_disposeDiagnostic(diag);
        }

        let mut decls: Vec<(CXCursorKind, String)> = Vec::new();
        clang_visitChildren(
            clang_getTranslationUnitCursor(tu),
            Some(collect),
            &mut decls as *mut _ as CXClientData,
        );
        clang_disposeTranslationUnit(tu);
        clang_disposeIndex(index);
        (decls, errors)
    }
}

#[test]
fn version() {
    let version = unsafe { into_string(clang_getClangVersion()) };
    assert!(version.contains("clang version"), "{version}");
    if let Some(llvm) = VENDORED_LLVM_VERSION {
        assert!(version.contains(llvm), "{version} is not {llvm}");
    }
}

/// The `clang_<major>_0` features, newest first, and whether each one is
/// enabled (by Cargo, or by the build script for a detected system libclang).
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

/// The version whose API is declared, if a version feature is enabled.
fn feature_major() -> Option<u32> {
    VERSION_FEATURES
        .iter()
        .find_map(|&(major, enabled)| enabled.then_some(major))
}

fn major_of(version: &str) -> u32 {
    version.split('.').next().unwrap().parse().unwrap()
}

/// With `vendored`, the LLVM release that is built is the one whose API is
/// declared: that of the highest enabled `clang_<major>_0` feature, or the
/// newest supported release without one.
#[test]
fn vendored_release_matches_features() {
    let Some(version) = VENDORED_LLVM_VERSION else {
        return;
    };
    let declared = feature_major().unwrap_or(VERSION_FEATURES[0].0);
    assert_eq!(major_of(version), declared, "built LLVM {version}");
}

/// Without `vendored` and a version feature, the build script declares the
/// API of the system libclang's version, which it detects. It has to be the
/// libclang that is loaded.
#[test]
fn declared_api_matches_the_detected_libclang() {
    let Some(detected) = option_env!("CLANG_RS_SYS_DETECTED_LLVM_VERSION") else {
        return;
    };
    // Such as "Ubuntu clang version 19.1.1 (1ubuntu1~24.04.2)".
    let loaded = unsafe { into_string(clang_getClangVersion()) };
    let loaded_major = loaded
        .split("version ")
        .nth(1)
        .map(major_of)
        .unwrap_or_else(|| panic!("no version in {loaded:?}"));
    assert_eq!(
        loaded_major,
        major_of(detected),
        "detected {detected}, loaded {loaded}"
    );

    let oldest = VERSION_FEATURES[VERSION_FEATURES.len() - 1].0;
    let newest = VERSION_FEATURES[0].0;
    assert_eq!(
        feature_major(),
        Some(loaded_major.clamp(oldest, newest)),
        "the features of libclang {detected} aren't enabled"
    );
}

#[test]
fn parses_c_with_builtin_headers() {
    let source = r#"
        #include <stddef.h>
        #include <stdint.h>
        #include <stdbool.h>
        #include <stdarg.h>

        struct point { int32_t x, y; };
        enum color { RED, GREEN };
        size_t area(struct point p) { return (size_t)(p.x * p.y); }
        bool sum(int n, ...) { va_list ap; va_start(ap, n); va_end(ap); return n > 0; }
    "#;
    let (decls, errors) = parse("test.c", source, &["-std=c11"]);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}");
    for (kind, name) in [
        (CXCursor_StructDecl, "point"),
        (CXCursor_EnumDecl, "color"),
        (CXCursor_FunctionDecl, "area"),
        (CXCursor_FunctionDecl, "sum"),
    ] {
        assert!(
            decls.iter().any(|(k, n)| *k == kind && n == name),
            "{name} not found in {decls:?}"
        );
    }
}

#[test]
fn parses_cxx() {
    let source = r#"
        namespace geometry {
        template <typename T> struct vec2 { T x, y; };
        constexpr int answer() { return 42; }
        }
        static_assert(geometry::answer() == 42, "constexpr evaluation");
    "#;
    let (decls, errors) = parse("test.cpp", source, &["-std=c++20", "-x", "c++"]);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}");
    assert!(decls
        .iter()
        .any(|(k, n)| *k == CXCursor_Namespace && n == "geometry"));
}

/// `install_builtin_headers` makes a working resource directory.
#[test]
#[cfg(feature = "vendored")]
fn installs_builtin_headers() {
    let dir = std::env::temp_dir().join(format!("clang-rs-sys-headers-{}", std::process::id()));
    if VENDORED_LLVM_VERSION.is_none() {
        // A system libclang, because of CLANG_RS_SYS_NO_VENDOR.
        let error = install_builtin_headers(&dir).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        return;
    }
    let resource_dir = format!("-resource-dir={}", dir.display());
    let source = "#include <stddef.h>\n#include <stdarg.h>\n#include <stdint.h>\nsize_t n;\n";

    // Without the headers, nothing else provides them (see `parse`).
    let (_, errors) = parse("test.c", source, &["-std=c11", &resource_dir]);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("'stddef.h' file not found")),
        "{errors:#?}"
    );

    install_builtin_headers(&dir).unwrap();
    // Again, with nothing left to write.
    install_builtin_headers(&dir).unwrap();

    let (decls, errors) = parse("test.c", source, &["-std=c11", &resource_dir]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}");
    assert!(decls
        .iter()
        .any(|(k, n)| *k == CXCursor_VarDecl && n == "n"));
}

#[test]
fn reports_errors() {
    let (_, errors) = parse("bad.c", "int f(void) { return undeclared; }", &[]);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].contains("undeclared"), "{errors:#?}");
}

#[test]
#[cfg(any(feature = "clang_17_0", not(feature = "clang_14_0")))]
fn index_options_match_the_c_layout() {
    use std::os::raw::c_uint;

    // CXIndexOptions has bit-fields, whose layout differs between the MSVC
    // and Itanium ABIs.
    let size = std::mem::size_of::<CXIndexOptions>();
    let expected = match (cfg!(target_env = "msvc"), cfg!(target_pointer_width = "64")) {
        (true, true) => 32,
        (true, false) => 20,
        (false, true) => 24,
        (false, false) => 16,
    };
    assert_eq!(size, expected);

    // libclang rejects options whose `Size` is not its own
    // sizeof(CXIndexOptions), which makes this a check against the C layout.
    unsafe {
        let mut options: CXIndexOptions = std::mem::zeroed();
        options.Size = size as c_uint;
        options.set_ExcludeDeclarationsFromPCH(1);
        options.set_StorePreamblesInMemory(1);
        assert_eq!(options.ExcludeDeclarationsFromPCH(), 1);
        assert_eq!(options.DisplayDiagnostics(), 0);
        let index = clang_createIndexWithOptions(&options);
        assert!(
            !index.is_null(),
            "libclang rejected a CXIndexOptions of {size} bytes"
        );
        clang_disposeIndex(index);

        options.Size = (size + 8) as c_uint;
        assert!(clang_createIndexWithOptions(&options).is_null());
    }
}
