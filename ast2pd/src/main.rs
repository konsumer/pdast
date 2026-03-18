//! ast2pd — Convert a pdast JSON AST back to a PureData .pd patch file.
//!
//! Reads a JSON AST produced by pd2ast and writes a valid `.pd` file.
//! Together with pd2ast this enables a full roundtrip:
//!
//!   pd2ast patch.pd | ast2pd - -o out.pd
//!
//! Usage:
//!   ast2pd [OPTIONS] <AST.json | ->
//!
//! Options:
//!   -o, --output <FILE>   Write .pd output to FILE instead of stdout
//!   -h, --help            Print help

use std::io::Read;
use std::path::PathBuf;

use clap::Parser;
use pdast::{emit_patch, from_json};

#[derive(Parser, Debug)]
#[command(
    name = "ast2pd",
    version,
    about = "Convert a pdast JSON AST back to a PureData .pd patch",
    long_about = "Reads a JSON AST produced by pd2ast and writes a valid .pd file.\n\
                  Use '-' as the input path to read from stdin.\n\n\
                  Full roundtrip example:\n  \
                  pd2ast patch.pd | ast2pd - -o roundtripped.pd"
)]
struct Args {
    /// Path to the JSON AST file. Use '-' to read from stdin.
    ast: String,

    /// Write .pd output to FILE instead of stdout.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();

    // Read JSON input
    let json = if args.ast == "-" {
        let mut s = String::new();
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

    // Deserialise the AST
    let patch = from_json(&json).unwrap_or_else(|e| {
        eprintln!("Error parsing JSON AST: {e}");
        std::process::exit(1);
    });

    // Emit .pd text
    let pd_text = emit_patch(&patch);

    // Write output
    match &args.output {
        Some(path) => {
            std::fs::write(path, &pd_text).unwrap_or_else(|e| {
                eprintln!("Error writing {:?}: {e}", path);
                std::process::exit(1);
            });
        }
        None => print!("{pd_text}"),
    }
}
