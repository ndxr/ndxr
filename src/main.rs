//! ndxr CLI entry point.
//!
//! Provides subcommands for indexing, searching, serving MCP, project setup,
//! status inspection, file skeleton rendering, activity monitoring, and self-upgrade.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

/// Local-first context engine for AI coding agents.
#[derive(Parser)]
#[command(
    name = "ndxr",
    version,
    about = "Local-first context engine for AI coding agents"
)]
struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    command: Option<Commands>,
}

/// Available CLI subcommands.
#[derive(Subcommand)]
enum Commands {
    /// Index or update the current workspace (incremental).
    #[command(
        long_about = "Index or update the current workspace (incremental).\n\n\
                       On first run, indexes all supported source files. On subsequent runs,\n\
                       only processes files that have been added, changed, or deleted since\n\
                       the last index. Unchanged files are skipped via BLAKE3 hash comparison."
    )]
    Index {
        /// Show detailed output.
        #[arg(long)]
        verbose: bool,
    },

    /// Force full re-index (preserves session memory).
    #[command(long_about = "Force full re-index (preserves session memory).\n\n\
                       Clears all code tables (files, symbols, edges) and re-parses every\n\
                       source file from scratch. Session memory (observations, decisions,\n\
                       insights) is preserved across reindexes.")]
    Reindex {
        /// Show detailed output.
        #[arg(long)]
        verbose: bool,
    },

    /// Start MCP server on stdio.
    #[command(long_about = "Start MCP server on stdio.\n\n\
                       Launches the ndxr MCP server for Claude Code integration. The server\n\
                       communicates via JSON-RPC over stdin/stdout. Typically configured via\n\
                       'ndxr setup' rather than invoked directly.")]
    Mcp,

    /// Configure Claude Code integration (writes .mcp.json + CLAUDE.md).
    #[command(
        long_about = "Configure Claude Code integration.\n\n\
                       Creates two files in the workspace root:\n\n\
                       1. .mcp.json  -- MCP server configuration that tells Claude Code\n\
                          how to launch ndxr. If the file already exists, the ndxr entry\n\
                          is merged in and other servers are preserved.\n\n\
                       2. CLAUDE.md  -- Agent instructions that tell Claude Code to use\n\
                          ndxr tools before reading files. If the file already exists,\n\
                          the ndxr section is appended or replaced in-place. Existing\n\
                          content is never removed.\n\n\
                       Safe to run multiple times. Existing configuration is merged,\n\
                       not overwritten.",
        after_help = "EXAMPLES:\n\
                      \x20 ndxr setup                  # project scope (default)\n\
                      \x20 ndxr setup --scope user     # user scope (~/.claude.json)\n\n\
                      CREATED FILES (project scope):\n\
                      \x20 .mcp.json    MCP server config (merged with existing servers)\n\
                      \x20 CLAUDE.md    Agent instructions (ndxr section appended/replaced)"
    )]
    Setup {
        /// Scope: 'project' writes .mcp.json in workspace, 'user' writes ~/.claude.json.
        #[arg(long, default_value = "project")]
        scope: String,
    },

    /// Show index statistics.
    #[command(long_about = "Show index statistics.\n\n\
                       Displays file, symbol, edge, session, and observation counts,\n\
                       database size, and last index timestamp. Use --json for\n\
                       machine-readable output.")]
    Status {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Search the index.
    #[command(
        long_about = "Search the index using hybrid BM25 + TF-IDF + PageRank scoring.\n\n\
                       Automatically detects query intent (debug, test, refactor, etc.) and\n\
                       adjusts scoring weights accordingly. Uses auto-relaxation to avoid\n\
                       empty results -- progressively broadens the search if needed.",
        after_help = "EXAMPLES:\n\
                      \x20 ndxr search \"authentication middleware\"\n\
                      \x20 ndxr search \"validate token\" --intent debug --explain\n\
                      \x20 ndxr search \"database\" -n 5"
    )]
    Search {
        /// Search query.
        query: String,
        /// Maximum results.
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Override intent detection (debug, test, refactor, modify, understand, explore).
        #[arg(long)]
        intent: Option<String>,
        /// Show score breakdown.
        #[arg(long)]
        explain: bool,
    },

    /// Show file skeletons (signature-only structural outlines).
    #[command(
        long_about = "Show file skeletons (signature-only structural outlines).\n\n\
                       Renders the structural outline of source files without implementation\n\
                       bodies. Classes, methods, and functions are displayed with their\n\
                       signatures. Members are indented under their parent class/struct.",
        after_help = "EXAMPLES:\n\
                      \x20 ndxr skeleton src/auth.ts src/middleware.ts\n\
                      \x20 ndxr skeleton src/auth.ts --docs false"
    )]
    Skeleton {
        /// File paths (relative to workspace root).
        files: Vec<String>,
        /// Include docstrings.
        #[arg(long, default_value_t = true, num_args = 1)]
        docs: bool,
    },

    /// Show recent MCP tool activity.
    #[command(
        long_about = "Show recent MCP tool activity from the current session.\n\n\
                       Displays auto-captured observations showing which tools Claude Code\n\
                       has called, with timestamps and result summaries.",
        after_help = "EXAMPLES:\n\
                      \x20 ndxr activity            # Last 20 observations\n\
                      \x20 ndxr activity --limit 50 # Last 50\n\
                      \x20 ndxr activity --follow   # Live tail (watch mode)"
    )]
    Activity {
        /// Maximum number of entries to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Continuously watch for new activity (like tail -f).
        #[arg(long, short, default_value_t = false)]
        follow: bool,
    },

    /// Upgrade ndxr to the latest release.
    #[command(
        long_about = "Upgrade ndxr to the latest release.\n\n\
                       Checks for a newer version on GitHub, verifies the checksum,\n\
                       and replaces the current binary. Works from any directory.\n\n\
                       Use --check to only check without upgrading.\n\
                       Use --force to skip the confirmation prompt.",
        after_help = "EXAMPLES:\n\
                      \x20 ndxr upgrade              # interactive upgrade\n\
                      \x20 ndxr upgrade --check      # check only\n\
                      \x20 ndxr upgrade --force      # skip confirmation"
    )]
    Upgrade {
        /// Only check for updates, do not download or install.
        #[arg(long)]
        check: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Index { verbose }) => cmd_index(verbose),
        Some(Commands::Reindex { verbose }) => cmd_reindex(verbose),
        Some(Commands::Mcp) => cmd_mcp(),
        Some(Commands::Setup { scope }) => cmd_setup(&scope),
        Some(Commands::Status { json }) => cmd_status(json),
        Some(Commands::Search {
            query,
            limit,
            intent,
            explain,
        }) => cmd_search(&query, limit, intent.as_deref(), explain),
        Some(Commands::Skeleton { files, docs }) => cmd_skeleton(&files, docs),
        Some(Commands::Activity { limit, follow }) => cmd_activity(limit, follow),
        Some(Commands::Upgrade { check, force }) => {
            if cmd_upgrade(check, force)? {
                std::process::exit(1);
            }
            Ok(())
        }
        None => {
            print_quick_start();
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

/// Index or update the current workspace.
fn cmd_index(verbose: bool) -> Result<()> {
    let root = find_root()?;
    let config = ndxr::config::NdxrConfig::from_workspace(root);

    if verbose {
        eprintln!("Indexing workspace: {}", config.workspace_root.display());
    }

    let stats = ndxr::indexer::index(&config)?;

    println!(
        "Indexed {} files ({} new, {} deleted, {} skipped)",
        stats.files_indexed + stats.skipped,
        stats.files_indexed,
        stats.files_deleted,
        stats.skipped,
    );
    println!(
        "Symbols: {}  Edges: {}",
        stats.symbols_extracted, stats.edges_extracted
    );
    if stats.observations_marked_stale > 0 {
        println!(
            "Observations marked stale: {}",
            stats.observations_marked_stale
        );
    }
    println!("Duration: {} ms", stats.duration_ms);

    Ok(())
}

/// Force a complete re-index, preserving session memory.
fn cmd_reindex(verbose: bool) -> Result<()> {
    let root = find_root()?;
    let config = ndxr::config::NdxrConfig::from_workspace(root);

    if verbose {
        eprintln!("Re-indexing workspace: {}", config.workspace_root.display());
    }

    let stats = ndxr::indexer::reindex(&config)?;

    println!("Re-indexed {} files", stats.files_indexed);
    println!(
        "Symbols: {}  Edges: {}",
        stats.symbols_extracted, stats.edges_extracted
    );
    if stats.observations_marked_stale > 0 {
        println!(
            "Observations marked stale: {}",
            stats.observations_marked_stale
        );
    }
    println!("Duration: {} ms", stats.duration_ms);

    Ok(())
}

/// Start the MCP server on stdio.
fn cmd_mcp() -> Result<()> {
    let root = find_root()?;
    let config = ndxr::config::NdxrConfig::from_workspace(root);

    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    rt.block_on(ndxr::mcp::server::start_mcp_server(config))
}

/// Configure Claude Code integration for the workspace.
fn cmd_setup(scope: &str) -> Result<()> {
    match scope {
        "project" => setup_project_scope()?,
        "user" => setup_user_scope()?,
        other => bail!("unknown scope: {other}. Expected 'project' or 'user'."),
    }

    println!("ndxr setup complete (scope: {scope})");
    Ok(())
}

/// Show index statistics.
fn cmd_status(json: bool) -> Result<()> {
    let root = find_root()?;
    let config = ndxr::config::NdxrConfig::from_workspace(root);
    let conn = ndxr::storage::db::open_or_create(&config.db_path)?;

    let status = ndxr::status::collect_index_status(&conn, &config.db_path)?;

    let indexed_languages: Vec<String> =
        match conn.prepare("SELECT DISTINCT language FROM files ORDER BY language") {
            Ok(mut stmt) => {
                let rows = stmt
                    .query_map([], |row| row.get::<_, String>(0))
                    .context("query indexed languages")?;
                let mut languages = Vec::new();
                for row in rows {
                    match row {
                        Ok(lang) => languages.push(lang),
                        Err(e) => tracing::warn!("skipping corrupt language row: {e}"),
                    }
                }
                languages
            }
            Err(e) => {
                tracing::warn!("failed to query languages: {e}");
                Vec::new()
            }
        };
    let supported_count = ndxr::languages::all_languages().len();

    if json {
        let result = serde_json::json!({
            "files": status.file_count,
            "symbols": status.symbol_count,
            "edges": status.edge_count,
            "observations": status.observation_count,
            "sessions": status.session_count,
            "languages": indexed_languages,
            "languages_supported": supported_count,
            "oldest_index_at": status.oldest_indexed_at,
            "newest_index_at": status.newest_indexed_at,
            "db_size_bytes": status.db_size_bytes,
            "workspace_root": config.workspace_root.display().to_string(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    } else {
        println!("ndxr index status");
        println!("  Workspace: {}", config.workspace_root.display());
        println!("  Files:         {}", status.file_count);
        println!("  Symbols:       {}", status.symbol_count);
        println!("  Edges:         {}", status.edge_count);
        if indexed_languages.is_empty() {
            println!("  Languages:     none (0 of {supported_count} supported)");
        } else {
            println!(
                "  Languages:     {} ({} of {supported_count} supported)",
                indexed_languages.join(", "),
                indexed_languages.len()
            );
        }
        println!("  Sessions:      {}", status.session_count);
        println!("  Observations:  {}", status.observation_count);
        println!("  DB size:       {}", format_bytes(status.db_size_bytes));
        if let Some(newest) = status.newest_indexed_at {
            println!("  Last indexed:  {}", format_age(newest));
        } else {
            println!("  Last indexed:  never");
        }
    }

    Ok(())
}

/// Search the index.
fn cmd_search(query: &str, limit: usize, intent_str: Option<&str>, explain: bool) -> Result<()> {
    let root = find_root()?;
    let config = ndxr::config::NdxrConfig::from_workspace(root);
    let conn = ndxr::storage::db::open_or_create(&config.db_path)?;
    let graph = ndxr::graph::builder::build_graph(&conn)?;
    ndxr::graph::centrality::compute_and_store(&conn, &graph)?;

    let intent_override = intent_str.and_then(ndxr::graph::intent::parse_intent);

    let outcome = ndxr::capsule::relaxation::search_with_relaxation(
        &conn,
        &graph,
        query,
        limit,
        intent_override,
    )?;
    let results = outcome.results;

    if results.is_empty() {
        println!("No results found for: {query}");
        return Ok(());
    }

    for (i, result) in results.iter().enumerate() {
        println!("{}. {} ({})", i + 1, result.fqn, result.kind);
        println!(
            "   {}:{}..{}  score={:.4}",
            result.file_path, result.start_line, result.end_line, result.score
        );
        if let Some(ref sig) = result.signature {
            println!("   {sig}");
        }
        if explain {
            let w = &result.why;
            println!(
                "   bm25={:.3} tfidf={:.3} centrality={:.3} boost={:.3} intent={}",
                w.bm25, w.tfidf, w.centrality, w.intent_boost, w.intent
            );
            if !w.matched_terms.is_empty() {
                println!("   matched: {}", w.matched_terms.join(", "));
            }
            println!("   reason: {}", w.reason);
        }
    }

    Ok(())
}

/// Show file skeletons.
fn cmd_skeleton(files: &[String], include_docs: bool) -> Result<()> {
    let root = find_root()?;
    let config = ndxr::config::NdxrConfig::from_workspace(root);
    let conn = ndxr::storage::db::open_or_create(&config.db_path)?;

    let skeletons = ndxr::skeleton::reducer::render_skeletons(&conn, files, include_docs)?;

    if skeletons.is_empty() {
        println!("No symbols found for the specified files.");
        return Ok(());
    }

    for skel in &skeletons {
        println!(
            "--- {} ({} symbols, {} lines) ---",
            skel.path, skel.symbol_count, skel.original_lines
        );
        println!("{}", skel.content);
        println!();
    }

    Ok(())
}

/// Check for updates and optionally upgrade the binary.
///
/// Returns `Ok(true)` when no action was taken (update available but not applied),
/// which `main()` maps to exit code 1.
fn cmd_upgrade(check: bool, force: bool) -> Result<bool> {
    let status = ndxr::upgrade::check_for_update()?;

    println!("Current: v{}", status.current);
    println!("Latest:  v{}", status.latest);

    if !status.is_outdated {
        println!("Already up to date.");
        return Ok(false);
    }

    let Some(asset) = status.asset else {
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        println!("Update available: v{}", status.latest);
        println!("No pre-built binary for {platform}.");
        println!("To upgrade from source: cargo install --git git@github.com:ndxr/ndxr.git");
        return Ok(true);
    };

    println!("Update available: v{}", status.latest);

    if check {
        return Ok(true);
    }

    if !force {
        eprint!("Proceed with upgrade? [y/N] ");
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("Failed to read user input")?;
        if !input.trim().eq_ignore_ascii_case("y") {
            return Ok(true);
        }
    }

    println!("Downloading {}...", asset.name);
    let result = ndxr::upgrade::download_and_verify(&asset)?;
    if let Some(size) = result.download_size {
        println!("Downloaded {} ({})", asset.name, format_bytes(size));
    }
    println!("Verifying checksum... ok");

    print!("Replacing binary... ");
    ndxr::upgrade::replace_binary(&result.binary_path)?;
    println!("ok");

    // Clean up the temp file (best-effort).
    cleanup_temp_binary(&result.binary_path);

    println!("Upgraded to v{}", status.latest);
    Ok(false)
}

/// Best-effort cleanup of the temporary extracted binary and its parent directory.
fn cleanup_temp_binary(path: &Path) {
    let _ = std::fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Show recent MCP tool activity.
fn cmd_activity(limit: usize, follow: bool) -> Result<()> {
    let root = find_root()?;
    let config = ndxr::config::NdxrConfig::from_workspace(root);
    let conn = ndxr::storage::db::open_or_create(&config.db_path)?;

    if follow {
        return cmd_activity_follow(&conn, limit);
    }

    print_recent_activity(&conn, limit)
}

fn print_recent_activity(conn: &rusqlite::Connection, limit: usize) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT kind, headline, content, datetime(created_at, 'unixepoch', 'localtime') as time \
         FROM observations ORDER BY created_at DESC, id DESC LIMIT ?1",
    )?;

    #[allow(clippy::cast_possible_wrap)] // small display limit fits in i64
    let rows = stmt.query_map([limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut entries = Vec::new();
    for row in rows {
        let (kind, headline, content, time) = row?;
        entries.push((kind, headline, content, time));
    }
    entries.reverse(); // chronological order

    if entries.is_empty() {
        println!("No activity recorded yet.");
        return Ok(());
    }

    for (kind, headline, content, time) in &entries {
        let display = headline.as_deref().unwrap_or(content.as_str());
        let kind_tag = match kind.as_str() {
            "auto" => "tool",
            "warning" => "warn",
            other => other,
        };
        println!("{time}  [{kind_tag:>8}]  {display}");
    }

    Ok(())
}

fn cmd_activity_follow(conn: &rusqlite::Connection, initial_limit: usize) -> Result<()> {
    print_recent_activity(conn, initial_limit)?;

    let mut last_seen: i64 = conn.query_row(
        "SELECT COALESCE(MAX(created_at), 0) FROM observations",
        [],
        |row| row.get(0),
    )?;

    println!("\n--- watching for new activity (Ctrl+C to stop) ---\n");

    let mut stmt = conn.prepare(
        "SELECT kind, headline, content, datetime(created_at, 'unixepoch', 'localtime'), created_at \
         FROM observations WHERE created_at > ?1 ORDER BY created_at ASC, id ASC",
    )?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));

        let rows = stmt.query_map([last_seen], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;

        for row in rows {
            let (kind, headline, content, time, created_at) = row?;
            let display = headline.as_deref().unwrap_or(content.as_str());
            let kind_tag = match kind.as_str() {
                "auto" => "tool",
                "warning" => "warn",
                other => other,
            };
            println!("{time}  [{kind_tag:>8}]  {display}");
            last_seen = created_at;
        }
    }
}

// ---------------------------------------------------------------------------
// Setup helpers
// ---------------------------------------------------------------------------

/// Sets up project-scope configuration: `.mcp.json` and `CLAUDE.md`.
fn setup_project_scope() -> Result<()> {
    let root = find_root()?;

    // Write/merge .mcp.json
    let mcp_path = root.join(".mcp.json");
    write_mcp_json(&mcp_path)?;

    // Write/merge CLAUDE.md
    let claude_path = root.join("CLAUDE.md");
    write_claude_md(&claude_path)?;

    Ok(())
}

/// Sets up user-scope configuration: `~/.claude.json` and `CLAUDE.md` in workspace.
fn setup_user_scope() -> Result<()> {
    let root = find_root()?;

    let home = home_dir()?;
    let claude_json_path = home.join(".claude.json");
    write_mcp_json(&claude_json_path)?;

    // CLAUDE.md still goes in the workspace root.
    let claude_path = root.join("CLAUDE.md");
    write_claude_md(&claude_path)?;

    Ok(())
}

/// Writes or merges the ndxr MCP server configuration into a JSON file.
///
/// If the file exists, reads it, merges the ndxr entry into `mcpServers`,
/// and writes back. If it does not exist, creates it with just the ndxr entry.
fn write_mcp_json(path: &Path) -> Result<()> {
    let ndxr_entry = serde_json::json!({
        "command": "ndxr",
        "args": ["mcp"],
        "env": {}
    });

    let mut root_obj = if path.exists() {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str::<serde_json::Value>(&content)
            .with_context(|| format!("parse {}", path.display()))?
    } else {
        serde_json::json!({})
    };

    // Ensure mcpServers exists as an object.
    if !root_obj
        .get("mcpServers")
        .is_some_and(serde_json::Value::is_object)
    {
        root_obj["mcpServers"] = serde_json::json!({});
    }

    root_obj["mcpServers"]["ndxr"] = ndxr_entry;

    let output = serde_json::to_string_pretty(&root_obj)?;
    std::fs::write(path, format!("{output}\n"))
        .with_context(|| format!("write {}", path.display()))?;

    eprintln!("  Wrote {}", path.display());
    Ok(())
}

/// CLAUDE.md content for the ndxr section.
const CLAUDE_MD_SECTION: &str = "\
## ndxr context engine

ndxr indexes this codebase and provides you with only the relevant code for each task.

**Before modifying any file**, call `mcp__ndxr__run_pipeline` with a description of your task.
Do not read files directly unless run_pipeline tells you to.

### Intent

Pass `intent` to `run_pipeline` to get optimized context for your task:

| Intent | When to use | What it optimizes |
|--------|------------|-------------------|
| `debug` | Fixing bugs, errors, crashes | Error paths, high-connectivity code |
| `test` | Writing or finding tests | Test files, test fixtures |
| `refactor` | Restructuring, renaming | Public APIs, blast radius, callers |
| `modify` | Adding features, extending | Balanced text + semantic match |
| `understand` | Learning how code works | Documentation, module structure, entry points |
| `explore` | General browsing (default) | Documented, central code |

Example: `mcp__ndxr__run_pipeline({ task: \"fix the auth crash\", intent: \"debug\" })`

### Tools

- `mcp__ndxr__run_pipeline` -- use this FIRST for every task (pass intent for best results)
- `mcp__ndxr__get_context_capsule` -- follow-up context queries (also accepts intent)
- `mcp__ndxr__get_skeleton` -- get file signatures without bodies
- `mcp__ndxr__get_impact_graph` -- check blast radius before refactoring
- `mcp__ndxr__search_memory` -- search past session insights
- `mcp__ndxr__save_observation` -- save important decisions or insights
- `mcp__ndxr__search_logic_flow` -- trace execution paths between symbols
- `mcp__ndxr__get_session_context` -- review session history
- `mcp__ndxr__index_status` -- check if index is ready
- `mcp__ndxr__reindex` -- force full re-index when index is stale (after git checkout, branch switch)";

/// Section header used to locate the ndxr section in CLAUDE.md.
const SECTION_HEADER: &str = "## ndxr context engine";

/// Writes or merges the ndxr section into CLAUDE.md.
///
/// If the file exists and contains the section header, replaces that section.
/// If it exists without the header, appends the section.
/// If it does not exist, creates it.
fn write_claude_md(path: &Path) -> Result<()> {
    if path.exists() {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;

        let new_content = if let Some(start_idx) = content.find(SECTION_HEADER) {
            // Find the end of this section (next ## header or end of file).
            let section_end = content[start_idx + SECTION_HEADER.len()..]
                .find("\n## ")
                .map_or(content.len(), |offset| {
                    start_idx + SECTION_HEADER.len() + offset
                });

            let mut result = String::with_capacity(content.len());
            result.push_str(&content[..start_idx]);
            result.push_str(CLAUDE_MD_SECTION);
            result.push('\n');
            if section_end < content.len() {
                result.push_str(&content[section_end..]);
            }
            result
        } else {
            // Append the section.
            let mut result = content;
            if !result.ends_with('\n') {
                result.push('\n');
            }
            result.push('\n');
            result.push_str(CLAUDE_MD_SECTION);
            result.push('\n');
            result
        };

        std::fs::write(path, new_content).with_context(|| format!("write {}", path.display()))?;
    } else {
        std::fs::write(path, format!("{CLAUDE_MD_SECTION}\n"))
            .with_context(|| format!("write {}", path.display()))?;
    }

    eprintln!("  Wrote {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Finds the workspace root from the current directory.
fn find_root() -> Result<PathBuf> {
    ndxr::workspace::find_workspace_root(&std::env::current_dir()?)
}

/// Returns the user's home directory.
fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .context("HOME environment variable not set")
}

/// Formats a byte count into a human-readable string.
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;

    if bytes >= GIB {
        #[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for counts
        let val = bytes as f64 / GIB as f64;
        format!("{val:.1} GiB")
    } else if bytes >= MIB {
        #[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for counts
        let val = bytes as f64 / MIB as f64;
        format!("{val:.1} MiB")
    } else if bytes >= KIB {
        #[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for counts
        let val = bytes as f64 / KIB as f64;
        format!("{val:.1} KiB")
    } else {
        format!("{bytes} B")
    }
}

/// Formats a Unix timestamp as a human-readable age string.
fn format_age(timestamp: i64) -> String {
    #[allow(clippy::cast_sign_loss)] // unix_now() is always positive
    let now = ndxr::util::unix_now() as u64;

    #[allow(clippy::cast_sign_loss)] // clamped to 0 minimum
    let ts_unsigned = timestamp.max(0) as u64;
    let age_secs = now.saturating_sub(ts_unsigned);

    if age_secs < 60 {
        format!("{age_secs}s ago")
    } else if age_secs < 3600 {
        format!("{}m ago", age_secs / 60)
    } else if age_secs < 86400 {
        format!("{}h ago", age_secs / 3600)
    } else {
        format!("{}d ago", age_secs / 86400)
    }
}

/// Prints the quick start guide.
fn print_quick_start() {
    println!("ndxr v{}", env!("CARGO_PKG_VERSION"));
    println!("Local-first context engine for AI coding agents");
    println!();
    println!("USAGE:");
    println!("  ndxr <command> [options]");
    println!();
    println!("COMMANDS:");
    println!("  index      Index or update the current workspace (incremental)");
    println!("  reindex    Force full re-index (preserves session memory)");
    println!("  mcp        Start MCP server on stdio");
    println!("  setup      Configure Claude Code (.mcp.json + CLAUDE.md)");
    println!("  status     Show index statistics");
    println!("  search     Search the index");
    println!("  skeleton   Show file skeletons (signatures only)");
    println!("  activity   Show recent MCP tool activity");
    println!("  upgrade    Upgrade to the latest release");
    println!();
    println!("QUICK START:");
    println!("  1. cd your-project");
    println!("  2. ndxr setup              # writes .mcp.json + CLAUDE.md");
    println!("  3. ndxr index              # build the index");
    println!("  4. ndxr status             # verify the index");
    println!("  5. ndxr search \"auth flow\" # search the codebase");
    println!();
    println!("Run 'ndxr <command> --help' for detailed help on a command.");
}
