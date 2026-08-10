//! Compile the vendored Stockfish 11 sources plus the C-ABI shim into a
//! static library linked into this crate.
//!
//! Notes:
//! - `main.cpp` is excluded: the shim replicates its init sequence on a
//!   dedicated thread instead of taking over the process entry point.
//! - Defines mirror the portable 64-bit build from Stockfish's Makefile:
//!   `NDEBUG`, `IS_64BIT`, `USE_POPCNT`. Deliberately NO `USE_PEXT`/BMI2/AVX
//!   — the same code must build for aarch64-apple-ios / aarch64-apple-darwin
//!   (the `cc` crate handles the Apple cross-target sysroots itself).

use std::path::{Path, PathBuf};

fn cpp_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.extension().is_some_and(|ext| ext == "cpp")
            && path.file_name().is_some_and(|n| n != "main.cpp")
        {
            out.push(path);
        }
    }
}

fn main() {
    let vendor = Path::new("vendor/stockfish");
    let mut sources = Vec::new();
    cpp_sources(vendor, &mut sources);
    cpp_sources(&vendor.join("syzygy"), &mut sources);
    sources.push(PathBuf::from("src/shim.cpp"));

    let mut build = cc::Build::new();
    build
        .cpp(true) // links the C++ runtime (-lc++ on Apple targets)
        .std("c++17")
        .opt_level(3)
        .define("NDEBUG", None)
        .define("IS_64BIT", None)
        .define("USE_POPCNT", None)
        .warnings(false)
        .include(vendor);
    for src in &sources {
        build.file(src);
    }
    build.compile("stockfish11");

    println!("cargo:rerun-if-changed=src/shim.cpp");
    println!("cargo:rerun-if-changed=vendor/stockfish");
}
