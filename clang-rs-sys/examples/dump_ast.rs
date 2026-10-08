//! Parses a C, C++ or Objective-C file and prints the declarations it
//! contains.
//!
//! ```text
//! cargo run --example dump_ast --features vendored -- path/to/file.c [clang args...]
//! ```

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::process::ExitCode;
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

extern "C" fn visit(
    cursor: CXCursor,
    _parent: CXCursor,
    depth: CXClientData,
) -> CXChildVisitResult {
    unsafe {
        let location = clang_getCursorLocation(cursor);
        if clang_Location_isFromMainFile(location) == 0 {
            return CXChildVisit_Continue;
        }
        let depth = *(depth as *const usize);
        let mut line = 0;
        clang_getSpellingLocation(
            location,
            ptr::null_mut(),
            &mut line,
            ptr::null_mut(),
            ptr::null_mut(),
        );
        let kind = into_string(clang_getCursorKindSpelling(clang_getCursorKind(cursor)));
        let name = into_string(clang_getCursorSpelling(cursor));
        let ty = into_string(clang_getTypeSpelling(clang_getCursorType(cursor)));
        println!(
            "{:indent$}{kind} {name} : {ty} (line {line})",
            "",
            indent = depth * 2
        );

        let mut child_depth = depth + 1;
        clang_visitChildren(
            cursor,
            Some(visit),
            &mut child_depth as *mut usize as CXClientData,
        );
    }
    CXChildVisit_Continue
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(file) = args.next() else {
        eprintln!("usage: dump_ast <file> [clang arguments...]");
        return ExitCode::FAILURE;
    };
    let mut clang_args: Vec<CString> = args.map(|a| CString::new(a).unwrap()).collect();
    if let Some(dir) = VENDORED_RESOURCE_DIR {
        clang_args.push(CString::new(format!("-resource-dir={dir}")).unwrap());
    }
    let argv: Vec<*const c_char> = clang_args.iter().map(|a| a.as_ptr()).collect();
    let file = CString::new(file).unwrap();

    unsafe {
        println!("{}", into_string(clang_getClangVersion()));
        let index = clang_createIndex(0, 1);
        let tu = clang_parseTranslationUnit(
            index,
            file.as_ptr(),
            argv.as_ptr(),
            argv.len() as c_int,
            ptr::null_mut(),
            0,
            CXTranslationUnit_None,
        );
        if tu.is_null() {
            eprintln!("failed to parse {}", file.to_string_lossy());
            clang_disposeIndex(index);
            return ExitCode::FAILURE;
        }
        let mut depth = 0usize;
        clang_visitChildren(
            clang_getTranslationUnitCursor(tu),
            Some(visit),
            &mut depth as *mut usize as CXClientData,
        );
        clang_disposeTranslationUnit(tu);
        clang_disposeIndex(index);
    }
    ExitCode::SUCCESS
}
