//! pdast2faust — Convert a pdast JSON AST to Faust DSP code.
//!
//! Usage:
//!   pdast2faust [OPTIONS] <AST.json>
//!
//! The tool reads a JSON AST produced by pd2ast and generates a Faust .dsp file.
//!
//! Object templates are resolved from:
//!   1. User-supplied --lib dirs (searched in order)
//!   2. The built-in library (vanilla PD objects implemented in Faust)
//!
//! Each lib dir contains <pd-object-name>.dsp files with a `pdobj = ...;`
//! expression. The generator uses these expressions inline and wires the
//! signal graph using Faust's block-diagram composition operators.

mod faust_gen;

use std::path::PathBuf;

use clap::Parser;
use pdast::from_json;

#[derive(Parser, Debug)]
#[command(
    name = "pdast2faust",
    version,
    about = "Convert a pdast JSON AST to Faust DSP code",
    long_about = None,
)]
struct Args {
    /// Path to the JSON AST file (from pd2ast). Use '-' to read from stdin.
    ast: String,

    /// Additional library directories to search for object templates.
    /// Each directory should contain <pd-object-name>.dsp files.
    /// Searched before the built-in library.
    #[arg(short = 'L', long = "lib", value_name = "DIR")]
    lib_dirs: Vec<PathBuf>,

    /// Write Faust output to FILE instead of stdout.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,

    /// Suppress warnings.
    #[arg(short, long)]
    quiet: bool,
}

fn main() {
    let args = Args::parse();

    // Read JSON input
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

    // Parse the AST
    let patch = from_json(&json).unwrap_or_else(|e| {
        eprintln!("Error parsing JSON AST: {e}");
        std::process::exit(1);
    });

    // Generate Faust code
    let mut generator = faust_gen::FaustGenerator::new(args.lib_dirs);
    let faust_code = generator.generate(&patch.root);

    // Print warnings
    if !args.quiet {
        for w in &generator.warnings {
            eprintln!("warning: {w}");
        }
    }

    // Write output
    match &args.output {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &faust_code) {
                eprintln!("Error writing {:?}: {e}", path);
                std::process::exit(1);
            }
        }
        None => print!("{faust_code}"),
    }
}
