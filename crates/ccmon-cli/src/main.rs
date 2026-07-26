//! `ccmon` — the command line interface.
//!
//! Every command drains the spool and ingests before answering, so the CLI is
//! correct whether or not the desktop app is running. There is no daemon to
//! start and nothing to keep alive.

mod backup;
mod clipboard;
mod doctor;
mod ls;

use anyhow::{Context, Result};
use ccmon_core::{config::Config, db, ingest, report, store};
use chrono::Utc;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ccmon",
    version,
    about = "Monitor Claude Code sessions: live triage and weekly work reports.",
    long_about = "ccmon reads Claude Code's own on-disk artifacts to answer two questions:\n\
                  which session wants you right now, and what actually shipped this week.\n\n\
                  It makes no network calls, never invokes an LLM, and is read-only with\n\
                  respect to Claude Code's data."
)]
struct Cli {
    /// Print debug logs to stderr.
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Emit a work report for pasting into a chat.
    Report {
        /// monday | today | yesterday | week | Nd | YYYY-MM-DD
        #[arg(long, default_value = "monday")]
        since: String,
        /// End of the window: now | today | yesterday | Nd | YYYY-MM-DD.
        /// A bare date means the end of that day, so --since=2026-06-01
        /// --until=2026-06-30 covers all of June.
        #[arg(long, default_value = "now")]
        until: String,
        /// Only projects whose path contains this substring.
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "markdown")]
        format: Format,
        /// Copy to the clipboard instead of printing.
        #[arg(long)]
        copy: bool,
        /// Include sessions with no commits and no file edits.
        #[arg(long)]
        include_empty: bool,
    },

    /// List sessions grouped by state, most urgent first.
    Ls {
        /// Only this state (needs-action, working, needs-review, idle, dead, ended).
        #[arg(long)]
        state: Option<String>,
        /// Include ENDED and DEAD sessions.
        #[arg(long)]
        all: bool,
        /// Only projects whose path contains this substring.
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "text")]
        format: Format,
    },

    /// Re-run ingest.
    Reindex {
        /// Drop every derived table and rebuild from events plus transcripts.
        #[arg(long)]
        force: bool,
    },

    /// Check retention, discovery, and database health.
    Doctor,

    /// Archive Claude Code's transcripts before its cleanup deletes them.
    Backup {
        /// Destination directory. Defaults to ~/.claude-archive.
        #[arg(long)]
        dest: Option<std::path::PathBuf>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Text,
    Markdown,
    Json,
}

fn main() {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    if let Err(e) = run(cli) {
        eprintln!("ccmon: {e:#}");
        std::process::exit(1);
    }
}

fn init_logging(verbose: bool) {
    let level = if verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::WARN
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .init();
}

fn run(cli: Cli) -> Result<()> {
    let cfg = Config::load().context("loading config")?;
    Config::write_default_if_missing().ok();

    match cli.command {
        Command::Report {
            since,
            until,
            project,
            format,
            copy,
            include_empty,
        } => {
            let conn = db::open_default()?;
            ingest::run(&conn, &cfg).context("ingesting")?;
            let views = store::build_views(&conn, &cfg, Utc::now())?;

            let since = report::parse_since(&since)?;
            let until = report::parse_until(&until)?;
            // An inverted range silently reports nothing, which reads as "you
            // did no work" rather than "you typed the dates backwards".
            if until < since {
                anyhow::bail!(
                    "--until ({}) is before --since ({})",
                    until.format("%Y-%m-%d %H:%M"),
                    since.format("%Y-%m-%d %H:%M")
                );
            }

            let opts = report::ReportOptions {
                since,
                until,
                project,
                include_empty,
                include_ended: cfg.include_ended_in_report,
            };
            let r = report::build(&views, &cfg, &opts);

            let out = match format {
                Format::Json => serde_json::to_string_pretty(&r)?,
                _ => report::render_markdown(&r),
            };

            if copy {
                clipboard::copy(&out)?;
                println!(
                    "Copied the report to the clipboard: {} projects, {} sessions, {} commits.",
                    r.projects.len(),
                    r.total_sessions,
                    r.total_commits
                );
            } else {
                print!("{out}");
            }
            Ok(())
        }

        Command::Ls {
            state,
            all,
            project,
            format,
        } => {
            let conn = db::open_default()?;
            ingest::run(&conn, &cfg).context("ingesting")?;
            let views = store::build_views(&conn, &cfg, Utc::now())?;
            ls::render(&views, state.as_deref(), all, project.as_deref(), format)
        }

        Command::Reindex { force } => {
            let conn = db::open_default()?;
            let stats = if force {
                ingest::reindex(&conn, &cfg)?
            } else {
                ingest::run(&conn, &cfg)?
            };
            println!(
                "{} sessions across {} projects · {} transcripts parsed ({} seen) · {} commits · {} spool events",
                stats.sessions,
                stats.projects,
                stats.transcripts_parsed,
                stats.transcripts_seen,
                stats.commits,
                stats.spool.events_inserted
            );
            if stats.transcript_lines_skipped > 0 {
                println!(
                    "{} transcript lines were unparseable and skipped (rerun with --verbose for detail)",
                    stats.transcript_lines_skipped
                );
            }
            Ok(())
        }

        Command::Doctor => doctor::run(&cfg),
        Command::Backup { dest } => backup::run(&cfg, dest),
    }
}
