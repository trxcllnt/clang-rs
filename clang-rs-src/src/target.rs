//! Mapping of Rust target triples onto the names used by LLVM, CMake and
//! autotools.

/// A parsed Rust target triple such as `armv7-unknown-linux-musleabi`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetInfo {
    /// The full Rust target triple.
    pub triple: String,
    /// The architecture component exactly as written (`riscv64gc`, `armv7`, ...).
    pub arch: String,
    pub vendor: String,
    pub os: String,
    /// The environment/ABI component (`gnu`, `musl`, `musleabi`, `msvc`, or empty).
    pub env: String,
}

impl TargetInfo {
    pub fn parse(triple: &str) -> Result<TargetInfo, String> {
        let parts: Vec<&str> = triple.split('-').collect();
        let (arch, vendor, os, env) = match parts.as_slice() {
            [arch, vendor, os] => (*arch, *vendor, *os, ""),
            [arch, vendor, os, env] => (*arch, *vendor, *os, *env),
            _ => return Err(format!("unsupported target triple `{triple}`")),
        };
        Ok(TargetInfo {
            triple: triple.to_string(),
            arch: arch.to_string(),
            vendor: vendor.to_string(),
            os: os.to_string(),
            env: env.to_string(),
        })
    }

    pub fn is_msvc(&self) -> bool {
        self.env == "msvc"
    }

    pub fn is_windows(&self) -> bool {
        self.os == "windows"
    }

    pub fn is_apple(&self) -> bool {
        self.vendor == "apple"
    }

    pub fn is_musl(&self) -> bool {
        self.env.starts_with("musl")
    }

    /// The architecture as reported by `CARGO_CFG_TARGET_ARCH`.
    pub fn rust_arch(&self) -> &str {
        let a = self.arch.as_str();
        match a {
            "i386" | "i486" | "i586" | "i686" => "x86",
            "arm64" | "arm64e" => "aarch64",
            _ if a.starts_with("riscv64") => "riscv64",
            _ if a.starts_with("riscv32") => "riscv32",
            _ if a.starts_with("arm") || a.starts_with("thumb") => "arm",
            _ => a,
        }
    }

    /// The triple LLVM should treat as its host (and default target) triple.
    pub fn llvm_triple(&self) -> String {
        let arch = match self.arch.as_str() {
            // LLVM spells the Apple Silicon architecture `arm64`.
            "aarch64" if self.is_apple() => "arm64",
            a if a.starts_with("riscv64") => "riscv64",
            a if a.starts_with("riscv32") => "riscv32",
            a => a,
        };
        if self.is_apple() && self.os == "darwin" {
            return format!("{arch}-apple-darwin");
        }
        self.with_arch(arch)
    }

    /// The triple passed to autotools `configure --host`/`--build`.
    pub fn gnu_triple(&self) -> String {
        let arch = match self.arch.as_str() {
            a if a.starts_with("riscv64") => "riscv64",
            a if a.starts_with("riscv32") => "riscv32",
            "arm64" | "arm64e" => "aarch64",
            a => a,
        };
        self.with_arch(arch)
    }

    fn with_arch(&self, arch: &str) -> String {
        let mut s = format!("{arch}-{}-{}", self.vendor, self.os);
        if !self.env.is_empty() {
            s.push('-');
            s.push_str(&self.env);
        }
        s
    }

    /// The name of the LLVM backend that generates code for this target.
    pub fn llvm_backend(&self) -> Option<&'static str> {
        let a = self.arch.as_str();
        Some(match a {
            "x86_64" | "x86_64h" | "i386" | "i486" | "i586" | "i686" => "X86",
            "aarch64" | "aarch64_be" | "arm64" | "arm64e" | "arm64ec" => "AArch64",
            _ if a.starts_with("arm") || a.starts_with("thumb") => "ARM",
            _ if a.starts_with("riscv") => "RISCV",
            _ if a.starts_with("loongarch") => "LoongArch",
            "s390x" => "SystemZ",
            _ if a.starts_with("powerpc") => "PowerPC",
            _ if a.starts_with("mips") => "Mips",
            _ if a.starts_with("sparc") => "Sparc",
            _ if a.starts_with("wasm") => "WebAssembly",
            "hexagon" => "Hexagon",
            "bpfeb" | "bpfel" => "BPF",
            "avr" => "AVR",
            "msp430" => "MSP430",
            _ => return None,
        })
    }

    /// Value for `CMAKE_SYSTEM_NAME` when cross compiling.
    pub fn cmake_system_name(&self) -> &str {
        match self.os.as_str() {
            "linux" => "Linux",
            "darwin" | "macos" => "Darwin",
            "windows" => "Windows",
            "ios" => "iOS",
            "freebsd" => "FreeBSD",
            "netbsd" => "NetBSD",
            "openbsd" => "OpenBSD",
            "android" => "Android",
            other => other,
        }
    }

    /// Value for `CMAKE_SYSTEM_PROCESSOR` when cross compiling.
    pub fn cmake_system_processor(&self) -> &str {
        match (self.rust_arch(), self.os.as_str()) {
            ("aarch64", "darwin") => "arm64",
            ("x86_64", "windows") => "AMD64",
            ("aarch64", "windows") => "ARM64",
            ("x86", "windows") => "X86",
            ("x86", _) => "i686",
            ("arm", _) => self.arch.as_str(),
            (arch, _) => arch,
        }
    }

    /// Value for `CMAKE_OSX_ARCHITECTURES`.
    pub fn apple_arch(&self) -> &str {
        match self.arch.as_str() {
            "aarch64" => "arm64",
            a => a,
        }
    }

    /// Visual Studio generator platform (`-A`).
    pub fn msvc_platform(&self) -> Option<&'static str> {
        match self.rust_arch() {
            "x86_64" => Some("x64"),
            "aarch64" => Some("ARM64"),
            "x86" => Some("Win32"),
            _ => None,
        }
    }

    /// Prefixes of GCC cross toolchains for targets the `cc` crate has no
    /// (or only a partial) built-in mapping for. The first one found on `PATH`
    /// is used when the user did not configure a compiler explicitly.
    pub fn cross_prefixes(&self) -> &'static [&'static str] {
        match self.triple.as_str() {
            "armv7-unknown-linux-musleabi" => &[
                "armv7-linux-musleabi",
                "armv7l-linux-musleabi",
                "arm-linux-musleabi",
            ],
            "i686-unknown-linux-musl" => &["i686-linux-musl", "i586-linux-musl"],
            "loongarch64-unknown-linux-musl" => &["loongarch64-linux-musl"],
            "s390x-unknown-linux-musl" => &["s390x-linux-musl", "s390x-ibm-linux-musl"],
            "riscv64gc-unknown-linux-musl" => &["riscv64-linux-musl"],
            "aarch64-unknown-linux-musl" => &["aarch64-linux-musl"],
            "x86_64-unknown-linux-musl" => &["x86_64-linux-musl"],
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TargetInfo;

    fn t(s: &str) -> TargetInfo {
        TargetInfo::parse(s).unwrap()
    }

    #[test]
    fn triples() {
        let cases = [
            // (rust, llvm, gnu, backend, processor)
            (
                "aarch64-apple-darwin",
                "arm64-apple-darwin",
                "aarch64-apple-darwin",
                "AArch64",
                "arm64",
            ),
            (
                "x86_64-apple-darwin",
                "x86_64-apple-darwin",
                "x86_64-apple-darwin",
                "X86",
                "x86_64",
            ),
            (
                "aarch64-pc-windows-msvc",
                "aarch64-pc-windows-msvc",
                "aarch64-pc-windows-msvc",
                "AArch64",
                "ARM64",
            ),
            (
                "x86_64-pc-windows-msvc",
                "x86_64-pc-windows-msvc",
                "x86_64-pc-windows-msvc",
                "X86",
                "AMD64",
            ),
            (
                "aarch64-unknown-linux-musl",
                "aarch64-unknown-linux-musl",
                "aarch64-unknown-linux-musl",
                "AArch64",
                "aarch64",
            ),
            (
                "aarch64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
                "AArch64",
                "aarch64",
            ),
            (
                "armv7-unknown-linux-musleabi",
                "armv7-unknown-linux-musleabi",
                "armv7-unknown-linux-musleabi",
                "ARM",
                "armv7",
            ),
            (
                "i686-unknown-linux-musl",
                "i686-unknown-linux-musl",
                "i686-unknown-linux-musl",
                "X86",
                "i686",
            ),
            (
                "loongarch64-unknown-linux-musl",
                "loongarch64-unknown-linux-musl",
                "loongarch64-unknown-linux-musl",
                "LoongArch",
                "loongarch64",
            ),
            (
                "riscv64gc-unknown-linux-musl",
                "riscv64-unknown-linux-musl",
                "riscv64-unknown-linux-musl",
                "RISCV",
                "riscv64",
            ),
            (
                "s390x-unknown-linux-gnu",
                "s390x-unknown-linux-gnu",
                "s390x-unknown-linux-gnu",
                "SystemZ",
                "s390x",
            ),
            (
                "s390x-unknown-linux-musl",
                "s390x-unknown-linux-musl",
                "s390x-unknown-linux-musl",
                "SystemZ",
                "s390x",
            ),
            (
                "x86_64-unknown-linux-musl",
                "x86_64-unknown-linux-musl",
                "x86_64-unknown-linux-musl",
                "X86",
                "x86_64",
            ),
            (
                "x86_64-unknown-linux-gnu",
                "x86_64-unknown-linux-gnu",
                "x86_64-unknown-linux-gnu",
                "X86",
                "x86_64",
            ),
        ];
        for (rust, llvm, gnu, backend, processor) in cases {
            let info = t(rust);
            assert_eq!(info.llvm_triple(), llvm, "{rust}");
            assert_eq!(info.gnu_triple(), gnu, "{rust}");
            assert_eq!(info.llvm_backend(), Some(backend), "{rust}");
            assert_eq!(info.cmake_system_processor(), processor, "{rust}");
        }
    }

    #[test]
    fn predicates() {
        assert!(t("x86_64-pc-windows-msvc").is_msvc());
        assert!(t("armv7-unknown-linux-musleabi").is_musl());
        assert!(t("aarch64-apple-darwin").is_apple());
        assert_eq!(t("aarch64-apple-darwin").cmake_system_name(), "Darwin");
        assert_eq!(t("riscv64gc-unknown-linux-musl").rust_arch(), "riscv64");
        assert_eq!(t("armv7-unknown-linux-musleabi").rust_arch(), "arm");
        assert_eq!(t("i686-unknown-linux-musl").rust_arch(), "x86");
        assert!(TargetInfo::parse("bogus").is_err());
    }
}
