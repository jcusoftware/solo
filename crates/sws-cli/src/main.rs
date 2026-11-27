//! Unified CLI and stdio MCP server (`solo`) for Solo.

mod commands;
mod edit;
mod mcp;
mod preview;
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
    /// Serve compiled site output over local HTTP on 127.0.0.1 (localhost only).
    Preview {
        /// Directory containing compiled site files to serve.
        outdir: PathBuf,
        /// TCP port to bind on 127.0.0.1 (0 selects a random free port).
        #[arg(long, default_value_t = preview::DEFAULT_PREVIEW_PORT)]
        port: u16,
    },
    /// Editor reducer entry points: eject, assign stable IDs, apply a command batch, undo a transaction.
    Edit {
        #[command(subcommand)]
        action: EditAction,
    },
    /// Run the stdio JSON-RPC 2.0 MCP server.
    Mcp {
        /// Workspace root directory that MCP file reads are jailed to (defaults to current working directory).
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum EditAction {
    /// One-way lowering of a pack manifest (e.g. venue.json) to editable site IR.
    Eject {
        /// Pack manifest JSON (never modified).
        input: PathBuf,
        /// Destination for the lowered site IR.
        #[arg(long)]
        out: PathBuf,
    },
    /// Explicitly assign stable node IDs (`uid`) to a site IR file.
    Upgrade {
        /// Site IR JSON (modified only if --out names the same path).
        input: PathBuf,
        /// Destination for the upgraded IR.
        #[arg(long)]
        out: PathBuf,
        /// Optional destination for the JSON upgrade report.
        #[arg(long)]
        report: Option<PathBuf>,
        /// Proceed even if unsupported fields in the source would be dropped.
        #[arg(long)]
        allow_lossy: bool,
    },
    /// Apply a versioned command batch atomically to an upgraded site IR file.
    Apply {
        /// Upgraded site IR JSON.
        input: PathBuf,
        /// Command batch JSON (`{ "version": 1, "commands": [...] }`).
        batch: PathBuf,
        /// Destination for the edited IR.
        #[arg(long)]
        out: PathBuf,
        /// Optional destination for the transaction (forward, inverse, touched).
        #[arg(long)]
        transaction: Option<PathBuf>,
        /// Proceed even if unsupported fields in the source would be dropped.
        #[arg(long)]
        allow_lossy: bool,
    },
    /// Undo a recorded transaction (restore mode) against the IR it produced.
    Undo {
        /// Site IR JSON that the transaction produced (the `apply --out` file).
        input: PathBuf,
        /// Transaction JSON written by `apply --transaction`.
        transaction: PathBuf,
        /// Destination for the restored IR.
        #[arg(long)]
        out: PathBuf,
        /// Optional destination for the undo transaction (its inverse redoes the step).
        #[arg(long)]
        undo_transaction: Option<PathBuf>,
        /// Proceed even if unsupported fields in the source would be dropped.
        #[arg(long)]
        allow_lossy: bool,
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
        Command::Preview { outdir, port } => {
            commands::cmd_preview(&outdir, port)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Edit { action } => {
            match action {
                EditAction::Eject { input, out } => edit::cmd_edit_eject(&input, &out)?,
                EditAction::Upgrade {
                    input,
                    out,
                    report,
                    allow_lossy,
                } => edit::cmd_edit_upgrade(&input, &out, report.as_deref(), allow_lossy)?,
                EditAction::Apply {
                    input,
                    batch,
                    out,
                    transaction,
                    allow_lossy,
                } => edit::cmd_edit_apply(
                    &input,
                    &batch,
                    &out,
                    transaction.as_deref(),
                    allow_lossy,
                )?,
                EditAction::Undo {
                    input,
                    transaction,
                    out,
                    undo_transaction,
                    allow_lossy,
                } => edit::cmd_edit_undo(
                    &input,
                    &transaction,
                    &out,
                    undo_transaction.as_deref(),
                    allow_lossy,
                )?,
            }
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
