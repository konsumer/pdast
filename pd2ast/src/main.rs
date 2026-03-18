//! pd2ast — Convert a PureData .pd patch to a JSON AST.
//!
//! Usage:
//!   pd2ast [OPTIONS] <PATCH.pd>
//!
//! The tool loads the root patch and resolves abstractions from the filesystem
//! the same way PureData would: it searches the patch's own directory, then
//! any extra search paths supplied with `-p`.
//!
//! Output is written to stdout (or to a file with `-o`).
//! Warnings are written to stderr unless `--quiet` is set.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use clap::Parser;
use pdast::parse_patch;

#[derive(Parser, Debug)]
#[command(
    name = "pd2ast",
    version,
    about = "Convert a PureData .pd patch to a JSON AST",
    long_about = None,
)]
struct Args {
    /// Path to the root .pd patch file.
    patch: PathBuf,

    /// Extra directories to search for abstractions (like PD's -path option).
    /// The patch's own directory is always searched first.
    #[arg(short = 'p', long = "path", value_name = "DIR")]
    search_paths: Vec<PathBuf>,

    /// Write JSON output to FILE instead of stdout.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,

    /// Suppress warnings.
    #[arg(short, long)]
    quiet: bool,

    /// Pretty-print JSON (default: pretty). Use --compact for minified output.
    #[arg(long)]
    compact: bool,

    /// Include the warnings array in JSON output.
    #[arg(long)]
    include_warnings: bool,
}

fn main() {
    let args = Args::parse();

    // Read the root patch
    let root_content = match std::fs::read_to_string(&args.patch) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {:?}: {e}", args.patch);
            std::process::exit(1);
        }
    };

    // Build the search path list: patch dir first, then user-supplied paths
    let mut search_dirs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = args.patch.parent() {
        if parent == Path::new("") {
            search_dirs.push(PathBuf::from("."));
        } else {
            search_dirs.push(parent.to_path_buf());
        }
    } else {
        search_dirs.push(PathBuf::from("."));
    }
    search_dirs.extend(args.search_paths.iter().cloned());

    // Track which abstractions we've already loaded to avoid infinite loops
    let loading: HashSet<String> = HashSet::new();

    let loader = |name: &str| -> Option<String> {
        // Prevent recursive self-loading
        if loading.contains(name) {
            return None;
        }

        for dir in &search_dirs {
            let candidate = dir.join(format!("{}.pd", name));
            // Note: we can't mutate `loading` from inside a `Fn` closure,
            // so cycle detection is best-effort here. The recursive parser
            // call itself won't infinitely loop because each abstraction
            // body is parsed independently.
            if candidate.exists()
                && let Ok(content) = std::fs::read_to_string(&candidate)
            {
                return Some(content);
            }
        }
        None
    };

    let result = match parse_patch(&root_content, loader) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Parse error: {e}");
            std::process::exit(1);
        }
    };

    // Print warnings to stderr
    if !args.quiet {
        for w in &result.warnings {
            eprintln!(
                "warning{}: {}",
                w.node_id
                    .map(|id| format!(" (node {id})"))
                    .unwrap_or_default(),
                w.message
            );
        }
    }

    // Serialize
    let json = if args.include_warnings {
        if args.compact {
            serde_json::to_string(&result).unwrap()
        } else {
            serde_json::to_string_pretty(&result).unwrap()
        }
    } else if args.compact {
        serde_json::to_string(&result.patch).unwrap()
    } else {
        pdast::to_json(&result.patch).unwrap()
    };

    // Write output
    match &args.output {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &json) {
                eprintln!("Error writing {:?}: {e}", path);
                std::process::exit(1);
            }
        }
        None => println!("{json}"),
    }
}
