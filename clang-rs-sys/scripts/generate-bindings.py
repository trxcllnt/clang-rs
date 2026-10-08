#!/usr/bin/env python3
"""Regenerates src/bindings.rs and tests/link.rs.

    scripts/generate-bindings.py

The bindings are generated with bindgen from the clang-c headers of the newest
LLVM release that clang-rs-src supports. bindgen is run on the headers of
every older supported release as well, and every item (function, type or
constant) that is not the same in all of them is guarded by the
`clang_<major>_0` features, so that the crate declares exactly the API of the
release selected by the features, or of the newest release when no version
feature is enabled:

* Items introduced after the oldest supported release are only declared with
  the feature of the release that introduced them. Functions also have to be
  exported by the release (clang/tools/libclang/libclang.map).
* Items that were removed, or whose definition changed (such as constants
  whose value differs between releases), get a definition for every range of
  releases that has the same one.

The headers and symbol lists are fetched from GitHub. The result is checked
against the API of every release before it is written.

tests/link.rs takes the address of every declared function, so that linking it
fails if a function is declared that the libclang being linked doesn't have.

Needs bindgen-cli (`cargo install bindgen-cli`) and a libclang it can load
(set LIBCLANG_PATH if it is not found automatically).
"""

import re
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

CRATE = Path(__file__).resolve().parent.parent
RELEASES_SOURCE = CRATE.parent / "clang-rs-src" / "src" / "source.rs"
WRAPPER = CRATE / "wrapper.h"
BINDINGS = CRATE / "src" / "bindings.rs"
LINK_TEST = CRATE / "tests" / "link.rs"
RAW_URL = "https://raw.githubusercontent.com/llvm/llvm-project/llvmorg-{version}/{path}"

GUARD_COMMENT = """\
// Post-processed by scripts/generate-bindings.py: items that are not the same
// in every supported libclang release are guarded by the `clang_<major>_0`
// features. As the features are cumulative, `not(feature = "clang_{oldest}_0")`
// means that no version feature is enabled, in which case the API of the
// newest release is declared. (When a system libclang is linked, the build
// script sets the feature cfgs of its version.)
"""


def supported_releases():
    """The LLVM releases clang-rs-src can build, oldest first."""
    text = RELEASES_SOURCE.read_text()
    start = text.index("pub(crate) const LLVM_RELEASES")
    block = text[start : text.index("];", start)]
    return re.findall(r'llvm_release!\(\s*"(\d+\.\d+\.\d+)"', block)


def major(version):
    return int(version.split(".")[0])


def fetch(version, path):
    """A file of the release's source tree, or None if it has no such file."""
    url = RAW_URL.format(version=version, path=path)
    try:
        with urllib.request.urlopen(url, timeout=60) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def exported_functions(version):
    text = fetch(version, "clang/tools/libclang/libclang.map").decode()
    return set(re.findall(r"^\s*(clang_\w+);", text, re.M))


def wrapper_headers():
    return re.findall(r"#include <(clang-c/\w+\.h)>", WRAPPER.read_text())


def fetch_headers(version, include_dir):
    """Downloads the headers wrapper.h includes that the release has, and the
    headers they include. Returns the names of the downloaded headers."""
    wrapped = wrapper_headers()
    present, todo = set(), list(wrapped)
    while todo:
        name = todo.pop()
        if name in present:
            continue
        data = fetch(version, f"clang/include/{name}")
        if data is None:
            if name in wrapped:
                # Older releases don't have all of them.
                continue
            sys.exit(f"LLVM {version} has no {name}")
        path = include_dir / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        present.add(name)
        todo += [m.decode() for m in re.findall(rb'#include "(clang-c/\w+\.h)"', data)]
    return present


def run_bindgen(wrapper, include_dir, output):
    subprocess.run(
        [
            "bindgen",
            str(wrapper),
            "--output", str(output),
            "--rust-target", "1.74",
            "--allowlist-function", "clang_.*",
            "--allowlist-type", "CX.*",
            "--allowlist-var", "CINDEX_VERSION.*",
            "--blocklist-type", "__time_t|time_t",
            "--no-layout-tests",
            "--no-prepend-enum-name",
            "--merge-extern-blocks",
            "--sort-semantically",
            "--",
            f"-I{include_dir}",
        ],
        check=True,
    )


def fix_index_options(src):
    """CXIndexOptions contains bit-fields, which the Itanium C++ ABI (Linux,
    macOS) packs right after its two `char` members while MSVC starts a new
    4-byte unit, moving everything after them. bindgen computes the layout for
    one target only, so add the MSVC padding (as bindgen generates it for MSVC
    targets) behind a cfg."""
    if "pub struct CXIndexOptions {" not in src:
        return src
    anchor = (
        "    pub ThreadBackgroundPriorityForEditing: ::std::os::raw::c_uchar,\n"
        "    pub _bitfield_1: __BindgenBitfieldUnit<[u8; 2usize]>,\n"
    )
    if src.count(anchor) != 1:
        sys.exit("the layout of CXIndexOptions changed; update fix_index_options()")
    return src.replace(
        anchor,
        "    pub ThreadBackgroundPriorityForEditing: ::std::os::raw::c_uchar,\n"
        '    #[cfg(target_env = "msvc")]\n'
        "    pub __bindgen_padding_0: [u8; 2usize],\n"
        "    pub _bitfield_1: __BindgenBitfieldUnit<[u8; 2usize]>,\n",
    )


# Parsing of the (rustfmt-formatted) output of bindgen.

STRING = re.compile(r'"(?:[^"\\]|\\.)*"')
HEAD = re.compile(
    r"(?:pub )?(?:(const|type|struct|union|fn|static) (\w+)"
    r"|(impl)(?:<[^>]*>)? (?:[\w:]+ for )?(\w+))"
)


class Item:
    """An item with its attributes. `head` is the index of the line that
    declares it, after the attributes."""

    def __init__(self, kind, name, lines, head):
        self.kind = kind
        self.name = name
        self.lines = lines
        self.head = head
        # A type can have several impl blocks.
        self.key = (kind, name, lines[head].strip() if kind == "impl" else "")

    def documented(self):
        return any(line.lstrip().startswith("#[doc") for line in self.lines[: self.head])

    def cfgs(self):
        return [
            line.strip()[len("#[cfg(") : -len(")]")]
            for line in self.lines[: self.head]
            if line.lstrip().startswith("#[cfg(")
        ]

    def definition(self):
        """The item without documentation, its own cfg attributes and, for
        functions, parameter names (which some releases leave out)."""
        lines = [
            line
            for i, line in enumerate(self.lines)
            if not line.lstrip().startswith("#[doc")
            and not (i < self.head and line.lstrip().startswith("#[cfg("))
        ]
        text = "\n".join(lines)
        if self.kind == "fn":
            text = re.sub(r"\b\w+: ", "_: ", text)
        return text

    def with_guard(self, docs, cfg):
        indent = re.match(r"\s*", self.lines[self.head])[0]
        extra = []
        if docs and self.documented():
            extra.append(f'{indent}#[doc = ""]')
        extra += [f'{indent}#[doc = " {doc}"]' for doc in docs]
        if cfg:
            extra.append(f"{indent}#[cfg({cfg})]")
        return self.lines[: self.head] + extra + self.lines[self.head :]


class Block:
    """`extern "C" { ... }`."""

    def __init__(self, open_lines, entries, close_line):
        self.open_lines = open_lines
        self.entries = entries
        self.close_line = close_line


def depth_change(line):
    line = STRING.sub('""', line)
    return sum(map(line.count, "([{")) - sum(map(line.count, ")]}"))


def parse(lines, i=0, end=None):
    """Splits bindgen output into Items, Blocks and the lines between them."""
    end = len(lines) if end is None else end
    entries = []
    while i < end:
        line = lines[i]
        if not line.strip() or line.lstrip().startswith(("//", "/*")):
            entries.append(line)
            i += 1
            continue
        start = i
        while lines[i].lstrip().startswith("#["):
            i += 1
        head = i
        depth = 0
        while True:
            depth += depth_change(lines[i])
            if depth == 0 and lines[i].rstrip().endswith((";", "}")):
                break
            i += 1
        i += 1
        if lines[head].strip() == 'extern "C" {':
            block = Block(lines[start : head + 1], parse(lines, head + 1, i - 1), lines[i - 1])
            entries.append(block)
            continue
        m = HEAD.match(lines[head].strip())
        if not m:
            sys.exit(f"cannot parse the bindgen output at {lines[head]!r}")
        entries.append(Item(m[1] or m[3], m[2] or m[4], lines[start:i], head - start))
    return entries


def items(entries):
    """All Items, including those in Blocks, in order."""
    for entry in entries:
        if isinstance(entry, Block):
            yield from items(entry.entries)
        elif isinstance(entry, Item):
            yield entry


# Guards.


class Guards:
    def __init__(self, releases):
        self.releases = releases
        self.count = len(releases)

    def feature(self, i):
        return f'feature = "clang_{major(self.releases[i])}_0"'

    def cfg(self, a, b):
        """For the releases a..b-1 (indices; b == count: up to the newest)."""
        if b == self.count:
            # Also when no version feature is enabled.
            return None if a == 0 else f"any({self.feature(a)}, not({self.feature(0)}))"
        return f"all({self.feature(a)}, not({self.feature(b)}))"

    def describe(self, a, b):
        first = f"{major(self.releases[a])}.0"
        if b == self.count:
            return f"{first} and later"
        last = f"{major(self.releases[b - 1])}.0"
        if b - a == 1:
            return first
        return f"{first} and {last}" if b - a == 2 else f"{first} to {last}"


def ranges(key, apis, releases):
    """The ranges of releases (a, b, item) that have the same definition of
    the item, represented by the item of the range's last release."""
    result = []
    previous = None
    for i, release in enumerate(releases):
        item = apis[release].get(key)
        definition = item.definition() if item else None
        if definition is not None and definition == previous:
            a, _, _ = result[-1]
            result[-1] = (a, i + 1, item)
        elif item:
            result.append((i, i + 1, item))
        previous = definition
    return result


def guarded_variants(key, apis, releases, guards):
    """The lines of every definition of the item, with guards."""
    variants = ranges(key, apis, releases)
    if len(variants) == 1:
        a, b, item = variants[0]
        if a == 0 and b == guards.count:
            return [item.lines]
        docs = [f"Only available on `libclang` {guards.describe(a, b)}."]
        return [item.with_guard(docs, guards.cfg(a, b))]

    if key[0] == "const":
        values = ", ".join(
            f"{const_value(item)} for {guards.describe(a, b)}" for a, b, item in variants
        )
        docs = [f"Its value depends on the `libclang` version: {values}."]
        return [item.with_guard(docs, guards.cfg(a, b)) for a, b, item in variants]
    return [
        item.with_guard(
            [
                "Its definition depends on the `libclang` version; this is the one"
                f" for {guards.describe(a, b)}."
            ],
            guards.cfg(a, b),
        )
        for a, b, item in variants
    ]


def const_value(item):
    m = re.search(r"=\s*(.+?);\s*$", " ".join(item.definition().split()))
    return m[1]


def generate(newest_entries, apis, orders, releases, guards):
    """The newest release's bindings with guards, plus the items that only
    older releases have. Those are placed before the item that follows them
    in the last release that has them (functions without one at the end of
    the extern block, other items at the end)."""
    newest = apis[releases[-1]]
    before, block_end, end = {}, [], []
    placed = set()
    for release in reversed(releases):
        order = orders[release]
        for i, key in enumerate(order):
            if key in newest or key in placed or key not in apis[release]:
                continue
            placed.add(key)
            anchor = next((k for k in order[i + 1 :] if k in newest), None)
            if anchor is not None and (anchor[0] == "fn") == (key[0] == "fn"):
                before.setdefault(anchor, []).append(key)
            else:
                (block_end if key[0] == "fn" else end).append(key)

    def emit(keys, out):
        for key in keys:
            for lines in guarded_variants(key, apis, releases, guards):
                out += lines

    def emit_entries(entries, out):
        for entry in entries:
            if isinstance(entry, Block):
                out += entry.open_lines
                emit_entries(entry.entries, out)
                emit(block_end, out)
                out.append(entry.close_line)
            elif isinstance(entry, Item):
                emit(before.get(entry.key, []), out)
                emit([entry.key], out)
            else:
                out.append(entry)

    out = []
    emit_entries(newest_entries, out)
    emit(end, out)
    return out


# Checks.


def cfg_active(expr, features):
    tokens = re.findall(r'"[^"]*"|\w+|[(),=]', expr)
    pos = 0

    def predicate():
        nonlocal pos
        name = tokens[pos]
        pos += 1
        if tokens[pos] == "=":
            value = tokens[pos + 1].strip('"')
            pos += 2
            if name != "feature":
                sys.exit(f"unexpected cfg {expr}")
            return value in features
        if tokens[pos] != "(" or name not in ("any", "all", "not"):
            sys.exit(f"unexpected cfg {expr}")
        pos += 1
        args = []
        while tokens[pos] != ")":
            args.append(predicate())
            if tokens[pos] == ",":
                pos += 1
        pos += 1
        if name == "not":
            return not args[0]
        return any(args) if name == "any" else all(args)

    result = predicate()
    if pos != len(tokens):
        sys.exit(f"unexpected cfg {expr}")
    return result


def check(lines, apis, releases):
    """Checks that the generated bindings declare exactly the API of each
    release for its feature (and of the newest one without features)."""
    entries = parse(lines)
    configs = [
        (f"clang_{major(release)}_0", release, releases[: i + 1])
        for i, release in enumerate(releases)
    ]
    configs.append(("no version feature", releases[-1], []))
    for name, release, enabled in configs:
        features = {f"clang_{major(r)}_0" for r in enabled}
        declared = {}
        for item in items(entries):
            if all(cfg_active(cfg, features) for cfg in item.cfgs()):
                if item.key in declared:
                    sys.exit(f"{item.name} is declared twice with {name}")
                declared[item.key] = item.definition()
        expected = {key: item.definition() for key, item in apis[release].items()}
        problems = [f"missing {key[1]}" for key in expected if key not in declared]
        problems += [f"extra {key[1]}" for key in declared if key not in expected]
        problems += [
            f"different {key[1]}"
            for key in expected
            if key in declared and declared[key] != expected[key]
        ]
        if problems:
            sys.exit(f"with {name}, the API differs from libclang {release}: {problems}")


def link_test(lines):
    out = [
        "//! Generated by scripts/generate-bindings.py.",
        "//!",
        "//! Takes the address of every function declared for the enabled",
        "//! `clang_<major>_0` features, so that this test fails to link if the",
        "//! libclang it is linked with lacks any of them.",
        "",
        "use std::hint::black_box;",
        "",
        "use clang_rs_sys::*;",
        "",
        "#[test]",
        "fn every_declared_function_links() {",
    ]
    for item in items(parse(lines)):
        if item.kind == "fn":
            out += [f"    #[cfg({cfg})]" for cfg in item.cfgs()]
            out.append(f"    black_box({item.name} as *const ());")
    out.append("}")
    return out


def main():
    if len(sys.argv) != 1:
        sys.exit(__doc__)
    releases = supported_releases()
    newest = releases[-1]

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)

        def prepare(release):
            include_dir = tmp / release / "include"
            present = fetch_headers(release, include_dir)
            return release, present, exported_functions(release)

        with ThreadPoolExecutor(len(releases)) as pool:
            fetched = list(pool.map(prepare, releases))

        apis, orders = {}, {}
        newest_entries = None
        for release, present, exported in fetched:
            include_dir = tmp / release / "include"
            if release == newest:
                missing = [h for h in wrapper_headers() if h not in present]
                if missing:
                    sys.exit(f"LLVM {newest} lacks {missing}; update wrapper.h")
                wrapper = WRAPPER
            else:
                wrapper = tmp / release / "wrapper.h"
                wrapper.write_text(
                    "".join(f"#include <{h}>\n" for h in wrapper_headers() if h in present)
                )
            output = tmp / release / "bindings.rs"
            run_bindgen(wrapper, include_dir, output)
            entries = parse(fix_index_options(output.read_text()).splitlines())
            declared = {}
            for item in items(entries):
                if item.key in declared:
                    sys.exit(f"bindgen declared {item.name} twice for LLVM {release}")
                declared[item.key] = item
            orders[release] = list(declared)
            if release == newest:
                newest_entries = entries
                functions = {key[1] for key in declared if key[0] == "fn"}
                unexported = sorted(functions - exported)
                if unexported:
                    sys.exit(f"not exported by libclang {newest}: {unexported}")
                undeclared = sorted(exported - functions)
                if undeclared:
                    sys.exit(f"exported by libclang {newest} but not declared: {undeclared}")
            apis[release] = {
                key: item
                for key, item in declared.items()
                if item.kind != "fn" or item.name in exported
            }

    guards = Guards(releases)
    lines = generate(newest_entries, apis, orders, releases, guards)
    if not lines[0].startswith("/* automatically generated by rust-bindgen"):
        sys.exit(f"unexpected first line {lines[0]!r}")
    lines[1:1] = GUARD_COMMENT.format(oldest=major(releases[0])).splitlines()
    check(lines, apis, releases)

    BINDINGS.write_text("\n".join(lines) + "\n")
    LINK_TEST.write_text("\n".join(link_test(lines)) + "\n")

    guarded = {}
    for item in items(parse(lines)):
        if item.cfgs():
            guarded[item.kind] = guarded.get(item.kind, 0) + 1
    total = sum(1 for _ in items(newest_entries))
    print(f"{total} items in libclang {newest}; guarded definitions: {guarded}")


if __name__ == "__main__":
    main()
