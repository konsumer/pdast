//! pdast2wclap — Convert a pdast JSON AST to a self-contained CLAP-wasm
//! (WCLAP) plugin, written in C.
//!
//! Usage:
//!   pdast2wclap [OPTIONS] <AST.json>
//!
//! The tool reads a JSON AST produced by pd2ast and generates one C source
//! file implementing the fixed `pd_*` DSP-unit ABI (see pd_wclap.h). That
//! file is meant to be compiled together with a small hand-written CLAP
//! "runtime shim" that provides the actual clap_entry/plugin surface — see
//! poketrack's plugins/pd2wclap/ for a working example of such a shim and
//! its build script.

mod wclap_gen;

use std::path::PathBuf;

use clap::Parser;
use pdast::from_json;

#[derive(Parser, Debug)]
#[command(
    name = "pdast2wclap",
    version,
    about = "Convert a pdast JSON AST to a self-contained CLAP-wasm plugin (C)",
    long_about = None,
)]
struct Args {
    /// Path to the JSON AST file (from pd2ast). Use '-' to read from stdin.
    ast: String,

    /// Write C output to FILE instead of stdout.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,

    /// Suppress warnings.
    #[arg(short, long)]
    quiet: bool,
}

fn main() {
    let args = Args::parse();

    let json = if args.ast == "-" {
        let mut s = String::new();
        use std::io::Read;
        std::io::stdin().read_to_string(&mut s).unwrap_or_else(|e| {
            eprintln!("Error reading stdin: {e}");
            std::process::exit(1);
        });
        s
    } else {
        std::fs::read_to_string(&args.ast).unwrap_or_else(|e| {
            eprintln!("Error reading {:?}: {e}", args.ast);
            std::process::exit(1);
        })
    };

    let patch = from_json(&json).unwrap_or_else(|e| {
        eprintln!("Error parsing JSON AST: {e}");
        std::process::exit(1);
    });

    let mut generator = wclap_gen::WclapGenerator::new();
    let c_code = generator.generate(&patch.root);

    if !args.quiet {
        for w in &generator.warnings {
            eprintln!("warning: {w}");
        }
    }

    match &args.output {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &c_code) {
                eprintln!("Error writing {:?}: {e}", path);
                std::process::exit(1);
            }
        }
        None => print!("{c_code}"),
    }
}
