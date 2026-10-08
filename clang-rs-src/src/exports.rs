//! Computes the link closure of libclang from the CMake export files that
//! LLVM and Clang write into their build trees (`LLVMExports.cmake` and
//! `ClangTargets.cmake`).
//!
//! Every exported static library lists its direct dependencies in
//! `INTERFACE_LINK_LIBRARIES`; following them from the libclang target yields
//! exactly the libraries libclang needs, in an order that satisfies
//! single-pass linkers, plus the system libraries they require.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::util::{self, Result};

#[derive(Debug, Default, Clone)]
pub(crate) struct ExportedTarget {
    /// `STATIC`, `SHARED`, `INTERFACE`, `UNKNOWN`, `EXECUTABLE`, ...
    pub kind: String,
    pub location: Option<PathBuf>,
    pub link: Vec<String>,
}

/// A dependency that is not one of the exported LLVM/Clang libraries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum External {
    /// A library linked by name (`-lpthread`, `rt`, `psapi`, ...).
    Lib(String),
    /// An Apple framework.
    Framework(String),
    /// A library given by path.
    Path(PathBuf),
    /// An imported target defined outside the export files (`FFI::ffi`).
    Imported(String),
}

#[derive(Debug)]
pub(crate) struct Closure {
    /// Exported static libraries, dependents before dependencies.
    pub libs: Vec<(String, PathBuf)>,
    pub externals: Vec<External>,
}

/// Parses CMake export files.
pub(crate) fn parse(files: &[PathBuf]) -> Result<HashMap<String, ExportedTarget>> {
    let mut targets: HashMap<String, ExportedTarget> = HashMap::new();
    for file in files {
        let text = util::read_to_string(file)?;
        parse_into(&text, &mut targets);
    }
    if targets.is_empty() {
        return Err(format!("no targets found in {files:?}"));
    }
    Ok(targets)
}

fn parse_into(text: &str, targets: &mut HashMap<String, ExportedTarget>) {
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("add_library(") {
            // add_library(LLVMSupport STATIC IMPORTED)
            let mut words = rest.trim_end_matches(')').split_whitespace();
            if let (Some(name), Some(kind)) = (words.next(), words.next()) {
                targets.entry(name.to_string()).or_default().kind = kind.to_string();
            }
        } else if let Some(rest) = line.strip_prefix("set_target_properties(") {
            // set_target_properties(LLVMSupport PROPERTIES
            //   INTERFACE_LINK_LIBRARIES "rt;dl;m;LLVMDemangle"
            // )
            let Some(name) = rest.split_whitespace().next() else {
                continue;
            };
            let target = targets.entry(name.to_string()).or_default();
            for prop in lines.by_ref() {
                let prop = prop.trim();
                if prop.starts_with(')') {
                    break;
                }
                let Some((key, value)) = prop.split_once(char::is_whitespace) else {
                    continue;
                };
                let value = unquote(value.trim());
                if key == "INTERFACE_LINK_LIBRARIES" {
                    target.link = split_list(&value);
                } else if key == "IMPORTED_LOCATION_RELEASE"
                    || (key.starts_with("IMPORTED_LOCATION") && target.location.is_none())
                {
                    target.location = Some(PathBuf::from(value));
                }
            }
        }
    }
}

/// Removes the quotes and CMake escapes from a quoted argument.
fn unquote(value: &str) -> String {
    let inner = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Splits a CMake list, keeping `;` inside generator expressions intact.
fn split_list(value: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '$' if chars.peek() == Some(&'<') => {
                depth += 1;
                current.push(c);
                current.push(chars.next().unwrap());
            }
            '>' if depth > 0 => {
                depth -= 1;
                current.push(c);
            }
            ';' if depth == 0 => items.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    items.push(current);
    items.retain(|s| !s.is_empty());
    items
}

/// Reduces a link item to a plain name, or `None` if it is a generator
/// expression that cannot be evaluated here.
fn simplify(item: &str) -> Option<&str> {
    let mut item = item.trim();
    while let Some(inner) = item
        .strip_prefix("$<LINK_ONLY:")
        .and_then(|s| s.strip_suffix('>'))
    {
        item = inner;
    }
    (!item.starts_with("$<") && !item.is_empty()).then_some(item)
}

fn classify(item: &str) -> Option<External> {
    if let Some(name) = item.strip_prefix("-l") {
        return Some(External::Lib(name.to_string()));
    }
    if item == "-pthread" {
        return Some(External::Lib("pthread".to_string()));
    }
    if let Some(name) = item.strip_prefix("-framework ") {
        return Some(External::Framework(name.trim().to_string()));
    }
    if item.starts_with('-') {
        // Other linker flags cannot be forwarded to dependents by Cargo.
        println!("cargo:warning=clang-rs-src: ignoring link flag `{item}` required by LLVM");
        return None;
    }
    if item.contains("::") {
        return Some(External::Imported(item.to_string()));
    }
    let path = Path::new(item);
    if path.is_absolute() || item.contains('/') || item.contains('\\') {
        return Some(External::Path(path.to_path_buf()));
    }
    let name = item.strip_suffix(".lib").unwrap_or(item);
    Some(External::Lib(name.to_string()))
}

/// Computes the link closure of `root`.
pub(crate) fn closure(targets: &HashMap<String, ExportedTarget>, root: &str) -> Result<Closure> {
    if !targets.contains_key(root) {
        return Err(format!("target `{root}` not found in the CMake exports"));
    }

    struct Walk<'a> {
        targets: &'a HashMap<String, ExportedTarget>,
        state: HashMap<&'a str, bool>, // false = in progress, true = done
        postorder: Vec<&'a str>,
        externals: Vec<External>,
        seen_externals: HashSet<External>,
    }

    impl<'a> Walk<'a> {
        fn visit(&mut self, name: &'a str) {
            match self.state.get(name) {
                Some(_) => return, // done, or a cycle (which static linking tolerates)
                None => self.state.insert(name, false),
            };
            let target = &self.targets[name];
            for item in &target.link {
                let Some(item) = simplify(item) else { continue };
                if let Some((dep, _)) = self.targets.get_key_value(item) {
                    self.visit(dep);
                } else if let Some(external) = classify(item) {
                    if self.seen_externals.insert(external.clone()) {
                        self.externals.push(external);
                    }
                }
            }
            self.state.insert(name, true);
            self.postorder.push(name);
        }
    }

    let mut walk = Walk {
        targets,
        state: HashMap::new(),
        postorder: Vec::new(),
        externals: Vec::new(),
        seen_externals: HashSet::new(),
    };
    walk.visit(root);

    let mut libs = Vec::new();
    for name in walk.postorder.iter().rev() {
        let target = &targets[*name];
        match target.kind.as_str() {
            "STATIC" | "UNKNOWN" => {
                let location = target.location.clone().ok_or_else(|| {
                    format!("the CMake exports do not record where `{name}` was built")
                })?;
                libs.push((name.to_string(), location));
            }
            "INTERFACE" => {}
            other => {
                return Err(format!(
                    "`{name}` is a {other} library; libclang can only be linked statically \
                     when all of its dependencies are static libraries"
                ))
            }
        }
    }
    Ok(Closure {
        libs,
        externals: walk.externals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LLVM: &str = r#"
# Create imported target LLVMDemangle
add_library(LLVMDemangle STATIC IMPORTED)

# Create imported target LLVMSupport
add_library(LLVMSupport STATIC IMPORTED)

set_target_properties(LLVMSupport PROPERTIES
  INTERFACE_LINK_LIBRARIES "rt;dl;-lpthread;m;LLVMDemangle"
)

add_library(LLVMCore STATIC IMPORTED)

set_target_properties(LLVMCore PROPERTIES
  INTERFACE_LINK_LIBRARIES "LLVMSupport;\$<LINK_ONLY:FFI::ffi>"
)

# Import target "LLVMDemangle" for configuration "Release"
set_property(TARGET LLVMDemangle APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(LLVMDemangle PROPERTIES
  IMPORTED_LINK_INTERFACE_LANGUAGES_RELEASE "CXX"
  IMPORTED_LOCATION_RELEASE "/b/lib/libLLVMDemangle.a"
  )
set_target_properties(LLVMSupport PROPERTIES
  IMPORTED_LOCATION_RELEASE "/b/lib/libLLVMSupport.a"
  )
set_target_properties(LLVMCore PROPERTIES
  IMPORTED_LOCATION_RELEASE "/b/lib/libLLVMCore.a"
  )
"#;

    const CLANG: &str = r#"
add_library(clangBasic STATIC IMPORTED)
set_target_properties(clangBasic PROPERTIES
  INTERFACE_LINK_LIBRARIES "LLVMSupport;LLVMCore"
)
add_library(libclang SHARED IMPORTED)
add_library(libclang_static STATIC IMPORTED)
set_target_properties(libclang_static PROPERTIES
  INTERFACE_COMPILE_DEFINITIONS "CINDEX_NO_EXPORTS"
  INTERFACE_LINK_LIBRARIES "clangBasic;dl;LLVMCore;LLVMSupport;\$<LINK_ONLY:\$<TARGET_PROPERTY:libclang,LINK_LIBRARIES>>"
)
set_target_properties(clangBasic PROPERTIES
  IMPORTED_LOCATION_RELEASE "/b/lib/libclangBasic.a"
  )
set_target_properties(libclang_static PROPERTIES
  IMPORTED_LOCATION_RELEASE "/b/lib/libclang.a"
  )
"#;

    #[test]
    fn computes_ordered_closure() {
        let mut targets = HashMap::new();
        parse_into(LLVM, &mut targets);
        parse_into(CLANG, &mut targets);
        let c = closure(&targets, "libclang_static").unwrap();
        let names: Vec<&str> = c.libs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "libclang_static",
                "clangBasic",
                "LLVMCore",
                "LLVMSupport",
                "LLVMDemangle"
            ]
        );
        assert_eq!(c.libs[0].1, PathBuf::from("/b/lib/libclang.a"));
        assert_eq!(
            c.externals,
            [
                External::Lib("rt".into()),
                External::Lib("dl".into()),
                External::Lib("pthread".into()),
                External::Lib("m".into()),
                External::Imported("FFI::ffi".into()),
            ]
        );
        assert!(closure(&targets, "libclang").is_err());
        assert!(closure(&targets, "nope").is_err());
    }

    #[test]
    fn lists() {
        assert_eq!(split_list("a;b;;c"), ["a", "b", "c"]);
        assert_eq!(
            split_list("a;$<$<CONFIG:Debug>:x;y>;b"),
            ["a", "$<$<CONFIG:Debug>:x;y>", "b"]
        );
        assert_eq!(unquote(r#""\$<LINK_ONLY:a>;b""#), "$<LINK_ONLY:a>;b");
        assert_eq!(simplify("$<LINK_ONLY:FFI::ffi>"), Some("FFI::ffi"));
        assert_eq!(simplify("$<LINK_ONLY:$<TARGET_PROPERTY:x,y>>"), None);
        assert_eq!(classify("psapi.lib"), Some(External::Lib("psapi".into())));
        assert_eq!(
            classify("/usr/lib/libz.a"),
            Some(External::Path("/usr/lib/libz.a".into()))
        );
    }
}
