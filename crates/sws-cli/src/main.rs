//! Unified CLI and stdio MCP server (`solo`) for Solo.

mod commands;
mod mcp;
mod templates;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "solo",
    version,
    about = "Solo — Rust static/edge site compiler, validator, and MCP server"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize a starter SWS project directory.
    Init {
        /// Target directory to create starter files in.
        dir: PathBuf,
        /// Optional vertical pack starter (e.g. "hospitality").
        #[arg(long)]
        pack: Option<String>,
    },
    /// Compile a site IR or pack manifest into deterministic static assets.
    Build {
        /// Input JSON file (SiteDocument IR or pack manifest such as venue.json).
        input: PathBuf,
        /// Output directory for compiled files.
        outdir: PathBuf,
        /// Verify existing files in <outdir> byte-for-byte without writing.
        #[arg(long)]
        check: bool,
    },
    /// Run IR validation, WCAG/compiler checks, plugin/ABC lint, and optional _headers parity.
    Check {
        /// Input JSON file (SiteDocument, venue.json, or partial JSON fixture).
        input: PathBuf,
        /// Optional Cloudflare _headers file to verify against SECURITY_HEADERS.
        #[arg(long)]
        headers_file: Option<PathBuf>,
        /// Emit a structured JSON check report.
        #[arg(long)]
        json: bool,
    },
    /// Export a deployable Cloudflare Workers + Static Assets + D1 bundle.
    Export {
        /// Input JSON file (SiteDocument IR or pack manifest such as venue.json).
        input: PathBuf,
        /// Output directory receiving public/, wrangler.jsonc, and migrations/0001_init.sql.
        outdir: PathBuf,
    },
    /// Run the stdio JSON-RPC 2.0 MCP server.
    Mcp {
        /// Workspace root directory that MCP file reads are jailed to (defaults to current working directory).
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

fn run() -> Result<ExitCode, String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { dir, pack } => {
            commands::cmd_init(&dir, pack.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Build {
            input,
            outdir,
            check,
        } => {
            commands::cmd_build(&input, &outdir, check)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Check {
            input,
            headers_file,
            json,
        } => {
            let ok = commands::cmd_check(&input, headers_file.as_deref(), json)?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Export { input, outdir } => {
            commands::cmd_export(&input, &outdir)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Mcp { root } => {
            let workspace_root = match root {
                Some(dir) => dir,
                None => std::env::current_dir()
                    .map_err(|e| format!("failed to resolve current directory: {e}"))?,
            };
            mcp::run_stdio(&workspace_root)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
