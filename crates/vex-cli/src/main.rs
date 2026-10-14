//! The `vex` binary: argument parsing, output formatting and exit codes.
//! Every command's logic lives in vex-app; this file only formats.

#[macro_use]
mod out;
mod cmd;
mod exit;
mod route;

use clap::{Parser, Subcommand};
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "vex",
    about = "Local code intelligence",
    disable_version_flag = true
)]
struct Cli {
    /// Print version, build and compiled-in grammars.
    #[arg(short = 'V', long, global = true)]
    version: bool,
    /// Stable machine output (schema 1). Never contains colour or progress.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum ModelsAction {
    /// The model, whether its weights are downloaded and verified, whether
    /// it is loaded, and its idle-unload timer.
    List,
    /// Download and verify the pinned weights now.
    Pull,
    /// Drop the loaded model now.
    Unload,
}

#[derive(Subcommand)]
enum HookAction {
    /// Chain vex into the repository's git hooks (never overwriting them).
    Install {
        /// Also sync before each commit.
        #[arg(long)]
        pre_commit: bool,
    },
    /// Remove vex from the hooks, leaving them as they were.
    Uninstall,
    /// Which hooks carry vex, and which manager owns them.
    Status,
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Pid, version, uptime, watched projects and loaded models.
    Status,
    /// Start the daemon in the background (no-op if it runs).
    Start,
    /// Stop the daemon.
    Stop,
    /// Stop the daemon and start a new one.
    Restart,
    /// Serve in the foreground (--detach: start in the background and return).
    #[command(hide = true)]
    Run {
        #[arg(long)]
        detach: bool,
    },
}

#[derive(Subcommand)]
enum Command {
    /// The background daemon that keeps indexes fresh and models warm.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Diagnose the setup; every failing check names its fix.
    Doctor,
    /// Register a project and build its first index. At a terminal it
    /// asks which agent to wire unless --agent says.
    Init {
        path: Option<std::path::PathBuf>,
        /// Wire an agent harness to `vex mcp` without asking: claude,
        /// codex, or all. Repeat for several.
        #[arg(long, value_parser = ["claude", "codex", "all"])]
        agent: Vec<String>,
        /// Index structure now and leave vectors for `vex embed`.
        #[arg(long)]
        defer_embed: bool,
    },
    /// Refresh a project's index (--force rebuilds it).
    Index {
        path: Option<std::path::PathBuf>,
        #[arg(long)]
        force: bool,
        /// Index structure now and leave vectors for `vex embed`.
        #[arg(long)]
        defer_embed: bool,
    },
    /// Fill the vectors a deferred or offline index lacks.
    Embed { path: Option<std::path::PathBuf> },
    /// Bring the index up to date now (the daemon does it in the
    /// background and this returns at once, unless --wait).
    Sync {
        path: Option<std::path::PathBuf>,
        /// Changed paths, as a hint (every change is found regardless).
        #[arg(long)]
        paths: Vec<String>,
        /// Refresh before returning, even with a daemon running.
        #[arg(long)]
        wait: bool,
        /// Only record HEAD (what a commit moves).
        #[arg(long)]
        record_head: bool,
        /// Run as the named git hook: quiet, bounded, always exit 0.
        #[arg(long)]
        hook: Option<String>,
    },
    /// Git hooks that keep the index in step with checkouts and commits.
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    /// The embedding model: its state, its weights, its idle timer.
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },
    /// Files, chunks, symbols, edges, freshness and indexed HEAD.
    Status { path: Option<std::path::PathBuf> },
    /// Remove a project's index and registry entry.
    Forget { path: Option<std::path::PathBuf> },
    /// Source of symbols or chunk ids; several targets share one budget.
    Expand {
        /// A symbol (name, qualified or dotted short form) or a chunk id.
        #[arg(required = true)]
        targets: Vec<String>,
        /// Treat a single target as a one-target list.
        #[arg(long)]
        list: bool,
        /// signature, minimal or full.
        #[arg(long, default_value = "full")]
        detail: String,
        /// The shared byte budget for a list (clamped to 2000..=48000).
        #[arg(long, default_value_t = vex_app::expand::DEFAULT_MAX_BYTES)]
        max_bytes: i64,
        /// The project (default: the current directory's).
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// A symbol's callers, callees and own source (or its control skeleton).
    Context {
        symbol: String,
        /// signature, minimal, full, or structure (the control skeleton).
        #[arg(long, default_value = "full")]
        detail: String,
        /// Hide name-match CALLS edges below this confidence (0 shows all).
        #[arg(long, default_value_t = 0.7)]
        confidence_min: f64,
        /// The project (default: the current directory's).
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// What a change to a symbol can affect.
    Impact {
        /// One or more symbols; several (or --list) report on each, and
        /// `both` treats them as one frontier.
        #[arg(required = true)]
        symbols: Vec<String>,
        /// Treat a single symbol as a one-symbol list.
        #[arg(long)]
        list: bool,
        /// up (what depends on it), down (what it uses) or both.
        #[arg(long, default_value = "up")]
        direction: String,
        /// Hops to follow (both: 1..=3).
        #[arg(long, default_value_t = 3)]
        depth: i64,
        /// For `both`: hide name-match CALLS edges below this confidence.
        #[arg(long, default_value_t = 0.7)]
        confidence_min: f64,
        /// The project (default: the current directory's).
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// Changed symbols and the callers they may break: the working tree
    /// against git's index, a base ref, or (--staged) git's index against
    /// HEAD. Exits 3 when the index is stale.
    Changes {
        /// Compare against this ref, adding each symbol's change shape and
        /// whether the change introduced each conditional.
        #[arg(long, default_value = "", conflicts_with = "staged")]
        base: String,
        /// Report what is staged for commit (git's index against HEAD).
        #[arg(long)]
        staged: bool,
        /// How many caller hops to follow upstream.
        #[arg(long, default_value_t = vex_app::changes::DEFAULT_MAX_DEPTH)]
        max_depth: i64,
        /// The project (default: the current directory's).
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// Every registered project: id, name, root and whether it still exists.
    Projects,
    /// Serve the query tools over MCP stdio (for agent harnesses).
    Mcp,
    /// The most-depended-on files and symbols (at most 12 files), packed
    /// to a token budget. Exits 3 when the index is stale.
    Map {
        /// Bias the ranking toward paths containing this text (repeatable).
        #[arg(long)]
        focus: Vec<String>,
        /// Approximate token budget for the map.
        #[arg(long, default_value_t = vex_app::map::DEFAULT_MAP_TOKEN_BUDGET)]
        budget_tokens: i64,
        /// The project (default: the current directory's).
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// Exact text search (ripgrep semantics), hits keyed by owning symbol.
    Grep {
        pattern: String,
        /// Only under this path (a directory or file, relative to the root).
        #[arg(long, default_value = "")]
        path: String,
        /// The pattern is literal text, not a regex.
        #[arg(long)]
        literal: bool,
        /// Matches per file, and (with context) lines in all (1..=200).
        #[arg(long, default_value_t = vex_app::grep::DEFAULT_MAX_RESULTS)]
        max_results: i64,
        /// Lines of context around each match (at most 40).
        #[arg(long, short = 'C', default_value_t = 0)]
        context_lines: i64,
        /// The project (default: the current directory's).
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// Search the index: one query, or several questions for one bundle
    /// of symbol leads to expand. Exits 3 when the index is stale.
    Search {
        /// The query; give several (or --list) for a lead bundle.
        #[arg(required = true)]
        queries: Vec<String>,
        /// Treat a single query as a one-question list.
        #[arg(long)]
        list: bool,
        #[arg(long, default_value_t = 5)]
        top_k: i64,
        /// Cap the response in bytes (0: no cap; a list defaults to 12000).
        #[arg(long, default_value_t = 0)]
        max_bytes: i64,
        /// Only results under this path (single query).
        #[arg(long, default_value = "")]
        path: String,
        /// Lead selector for a list: rules (default) or model.
        #[arg(long, default_value = "")]
        select: String,
        /// Refresh the index first when it is behind the working tree.
        #[arg(long)]
        force_fresh: bool,
        /// The project (default: the current directory's).
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// The exact extraction (Parsed) of one file; --json prints the
    /// canonical golden format.
    Parse {
        file: std::path::PathBuf,
        /// The directory paths are relative to (default: current directory).
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
}

fn main() {
    let argv: Vec<String> = match std::env::args_os().map(|a| a.into_string()).collect() {
        Ok(argv) => argv,
        // A non-UTF-8 argument cannot travel to the daemon; run here.
        Err(_) => std::process::exit(run_cli(Cli::parse())),
    };
    let cli = match Cli::try_parse_from(&argv) {
        Ok(c) => c,
        Err(e) => std::process::exit(usage(e)),
    };
    if routed(&cli)
        && let Some(code) = route::command(&argv)
    {
        std::process::exit(code);
    }
    std::process::exit(run_cli(cli));
}

/// Print a parse error (or --help) where the command's output goes.
fn usage(e: clap::Error) -> i32 {
    let text = e.render().to_string();
    if e.use_stderr() {
        out::write_err(text.as_bytes());
        exit::USAGE
    } else {
        out::write_out(text.as_bytes());
        exit::OK
    }
}

/// Run a full argument vector (what the daemon does for a client).
pub(crate) fn run_argv(argv: &[String]) -> i32 {
    match Cli::try_parse_from(argv) {
        Ok(cli) => run_cli(cli),
        Err(e) => usage(e),
    }
}

/// The path arguments a command was given.
fn path_args(cli: &Cli) -> Vec<&std::path::PathBuf> {
    let p = match &cli.command {
        Some(
            Command::Init { path, .. }
            | Command::Sync { path, .. }
            | Command::Index { path, .. }
            | Command::Embed { path }
            | Command::Status { path }
            | Command::Forget { path },
        ) => path,
        Some(
            Command::Expand { root, .. }
            | Command::Context { root, .. }
            | Command::Impact { root, .. }
            | Command::Changes { root, .. }
            | Command::Map { root, .. }
            | Command::Grep { root, .. }
            | Command::Search { root, .. },
        ) => root,
        _ => &None,
    };
    p.iter().collect()
}

/// Commands the daemon answers when it can: every query, and the
/// project commands that read or write an index. A path that does not
/// exist is reported in-process, so the message keeps its spelling.
fn routed(cli: &Cli) -> bool {
    !cli.version
        && path_args(cli).iter().all(|p| p.exists())
        // Index-building commands at a terminal run here, where they can
        // show progress (and init can ask which agent to wire).
        && !(matches!(
            cli.command,
            Some(Command::Init { .. } | Command::Index { .. } | Command::Embed { .. })
        ) && cmd::progress::wanted(cli.json))
        && matches!(
            cli.command,
            Some(
                Command::Init { .. }
                    | Command::Sync { hook: None, .. }
                    | Command::Index { .. }
                    | Command::Embed { .. }
                    | Command::Status { .. }
                    | Command::Forget { .. }
                    | Command::Expand { .. }
                    | Command::Context { .. }
                    | Command::Impact { .. }
                    | Command::Changes { .. }
                    | Command::Projects
                    | Command::Map { .. }
                    | Command::Grep { .. }
                    | Command::Search { .. }
            )
        )
}

fn run_cli(cli: Cli) -> i32 {
    if cli.version {
        let v = vex_app::version::version();
        if cli.json {
            print_json(&v);
        } else {
            let grammars = if v.grammars.is_empty() {
                "none".to_string()
            } else {
                v.grammars.join(", ")
            };
            outln!(
                "vex {} ({} {}-{})",
                v.version,
                v.build.profile,
                v.build.os,
                v.build.arch
            );
            outln!("grammars: {grammars}");
        }
        return exit::OK;
    }
    match cli.command {
        Some(Command::Doctor) => {
            let report = vex_app::doctor::doctor(vex_store_home().as_deref());
            if cli.json {
                print_json(&report);
            } else {
                for c in &report.checks {
                    outln!(
                        "{} {}: {}",
                        if c.ok { "ok  " } else { "FAIL" },
                        c.name,
                        c.detail
                    );
                    if let Some(fix) = &c.fix {
                        outln!("     fix: {fix}");
                    }
                }
            }
            if report.ok { exit::OK } else { exit::RUNTIME }
        }
        Some(Command::Init {
            path,
            agent,
            defer_embed,
        }) => {
            let mut agents: Vec<vex_app::agent::Agent> = Vec::new();
            for name in &agent {
                for a in vex_app::agent::Agent::parse(name).unwrap_or_default() {
                    if !agents.contains(&a) {
                        agents.push(a);
                    }
                }
            }
            let root = vex_app::project::project_root(&here(path.clone())).ok();
            let asked = cmd::setup::should_ask(cli.json, &agents, root.as_deref());
            if asked {
                agents = cmd::setup::ask();
            }
            // At a terminal a long embed is timed first and the person
            // chooses when it runs.
            let measured = !defer_embed && cmd::setup::should_ask_embed(cli.json);
            let choice = std::cell::Cell::new(None);
            let handed_off = std::cell::Cell::new(false);
            project_command(
                cli.json,
                |home| {
                    let plan = vex_app::embed::Plan::from_env(home, defer_embed);
                    let r = cmd::progress::shown(cli.json, || {
                        if measured {
                            vex_app::project::init_measured(
                                home,
                                &here(path),
                                &agents,
                                &plan,
                                cmd::setup::ASK_EMBED_OVER,
                                cmd::setup::ask_embed,
                            )
                            .map(|(r, c)| {
                                choice.set(c);
                                r
                            })
                        } else {
                            vex_app::project::init(home, &here(path), &agents, &plan)
                        }
                    })?;
                    if choice.get() == Some(vex_app::project::EmbedChoice::Background) {
                        handed_off.set(route::background_sync(&r.root));
                    }
                    Ok(r)
                },
                |r| {
                    outln!(
                        "{} project {} at {}",
                        r.registration,
                        r.project_id,
                        r.root.display()
                    );
                    print_index(&r.index);
                    for a in &r.agents {
                        outln!(
                            "{}: {} {} (runs `vex mcp`); skill {} {}",
                            a.agent,
                            a.config_action,
                            a.config.display(),
                            a.skill_action,
                            a.skill.display()
                        );
                    }
                    let pending = r.index.embed.as_ref().map_or(0, |e| e.pending);
                    match choice.get() {
                        Some(vex_app::project::EmbedChoice::Background) if handed_off.get() => {
                            outln!(
                                "the daemon is embedding the remaining {pending} chunks; `vex status` shows how far it got"
                            )
                        }
                        Some(vex_app::project::EmbedChoice::Background) => outln!(
                            "no daemon could take the remaining {pending} chunks; run `vex embed` to embed them"
                        ),
                        Some(vex_app::project::EmbedChoice::Later) => {
                            outln!("run `vex embed` to embed the remaining {pending} chunks")
                        }
                        _ => {}
                    }
                    if r.agents.is_empty()
                        && !asked
                        && vex_app::agent::configured(&r.root).is_empty()
                    {
                        errln!(
                            "vex: tip: `vex init --agent claude` (or codex, or all) wires your agent to vex"
                        );
                    }
                },
            )
        }
        Some(Command::Index {
            path,
            force,
            defer_embed,
        }) => project_command(
            cli.json,
            |home| {
                let plan = vex_app::embed::Plan::from_env(home, defer_embed);
                cmd::progress::shown(cli.json, || {
                    vex_app::project::index(home, &here(path), force, &plan)
                })
            },
            |r| {
                outln!("indexed {} ({})", r.root.display(), r.project_id);
                print_index(&r.index);
            },
        ),
        Some(Command::Models { action }) => match action {
            ModelsAction::List => project_command(
                cli.json,
                |home| {
                    Ok(vex_app::models::list(&vex_app::embed::Settings::from_env(
                        home,
                    )))
                },
                |r| {
                    for m in &r.models {
                        outln!(
                            "{} ({}, {} dims){}: {}, {}; idle unload after {}s",
                            m.name,
                            m.role,
                            m.dim,
                            if m.active { " [active]" } else { "" },
                            if m.verified {
                                "verified"
                            } else if m.downloaded {
                                "downloaded, NOT verified"
                            } else {
                                "not downloaded (`vex models pull`)"
                            },
                            if m.loaded { "loaded" } else { "not loaded" },
                            m.idle_unload_s
                        );
                    }
                    outln!("embedding backend: {}", r.embed_backend);
                },
            ),
            ModelsAction::Pull => project_command(
                cli.json,
                |home| vex_app::models::pull(&vex_app::embed::Settings::from_env(home)),
                |r| {
                    for f in &r.files {
                        outln!("{} {} ({} bytes)", f.action, f.name, f.bytes);
                    }
                    outln!("{} ready at {}", r.model, r.path.display());
                },
            ),
            ModelsAction::Unload => project_command(
                cli.json,
                |home| Ok(vex_app::models::unload(home)),
                |r| outln!("{}: {}", r.model, r.reason),
            ),
        },
        Some(Command::Sync {
            path,
            paths,
            wait,
            record_head,
            hook: Some(_),
        }) => cmd::hook::sync_as_hook(path, paths, wait, record_head),
        Some(Command::Sync {
            path,
            paths,
            wait,
            record_head,
            hook: None,
        }) => project_command(
            cli.json,
            |home| {
                let req = vex_app::sync::Request {
                    paths,
                    wait,
                    record_head,
                };
                vex_app::sync::sync(
                    home,
                    &here(path),
                    &req,
                    &vex_app::embed::Settings::from_env(home),
                )
            },
            |r| match (&r.index, &r.head) {
                (Some(i), _) => {
                    outln!("synced {} ({})", r.root.display(), r.mode);
                    print_index(i);
                }
                (None, Some(h)) => outln!("recorded HEAD {h} for {}", r.root.display()),
                (None, None) => outln!("sync {} for {}", r.mode, r.root.display()),
            },
        ),
        Some(Command::Hook { action }) => {
            let start = here(None);
            match action {
                HookAction::Install { pre_commit } => project_command(
                    cli.json,
                    |_| {
                        let exe = std::env::current_exe()
                            .and_then(std::fs::canonicalize)
                            .map_err(|e| vex_app::project::ProjectError::Store(e.to_string()))?;
                        vex_app::hooks::install(&start, &exe, pre_commit)
                    },
                    cmd::hook::print_report,
                ),
                HookAction::Uninstall => project_command(
                    cli.json,
                    |_| vex_app::hooks::uninstall(&start),
                    cmd::hook::print_report,
                ),
                HookAction::Status => project_command(
                    cli.json,
                    |_| vex_app::hooks::status(&start),
                    cmd::hook::print_report,
                ),
            }
        }
        Some(Command::Embed { path }) => project_command(
            cli.json,
            |home| {
                let settings = vex_app::embed::Settings::from_env(home);
                cmd::progress::shown(cli.json, || {
                    vex_app::embed::fill(home, &here(path), &settings)
                })
            },
            |r| {
                outln!(
                    "embedded {} chunks in {} ({} remaining; cache {}h/{}m)",
                    r.embedded,
                    r.root.display(),
                    r.remaining,
                    r.cache_hits,
                    r.cache_misses
                );
                if let Some(n) = &r.notice {
                    errln!("vex: {n}");
                }
            },
        ),
        Some(Command::Status { path }) => project_command(
            cli.json,
            |home| vex_app::project::status(home, &here(path)),
            |r| {
                outln!("{} ({})", r.root.display(), r.project_id);
                outln!(
                    "files {}, chunks {}, symbols {}, embedded {}",
                    r.files,
                    r.chunks,
                    r.symbols,
                    r.embedded_chunks
                );
                let edges: Vec<String> = r.edges.iter().map(|(t, n)| format!("{t} {n}")).collect();
                outln!(
                    "edges: {}",
                    if edges.is_empty() {
                        "none".into()
                    } else {
                        edges.join(", ")
                    }
                );
                outln!(
                    "indexed HEAD: {}",
                    r.indexed_head
                        .as_deref()
                        .unwrap_or("(not a git repository)")
                );
                outln!(
                    "{}",
                    if r.freshness.stale {
                        "stale: the working tree has moved since the last index"
                    } else {
                        "fresh"
                    }
                );
            },
        ),
        Some(Command::Forget { path }) => project_command(
            cli.json,
            |home| vex_app::project::forget(home, &here(path)),
            |r| {
                outln!(
                    "forgot {} ({}); removed {}",
                    r.root.display(),
                    r.project_id,
                    r.removed_index.display()
                );
            },
        ),
        Some(Command::Search {
            queries,
            list,
            top_k,
            max_bytes,
            path,
            select,
            force_fresh,
            root,
        }) => {
            let req = vex_app::search::Request {
                list: list || queries.len() > 1,
                queries,
                top_k,
                max_bytes,
                path,
                select,
                force_fresh,
            };
            let start = here(root);
            query_command(
                cli.json,
                |home| vex_app::search::run(home, &start, &req),
                |out| {
                    if let Some(n) = &out.notice {
                        errln!("vex: {n}");
                    }
                    print_answer(&out.answer, print_search)
                },
            )
        }
        Some(Command::Expand {
            targets,
            list,
            detail,
            max_bytes,
            root,
        }) => {
            let req = vex_app::expand::Request {
                list: list || targets.len() > 1,
                targets,
                detail,
                max_bytes,
            };
            let start = here(root);
            query_command(
                cli.json,
                |home| vex_app::expand::run(home, &start, &req),
                print_expand,
            )
        }
        Some(Command::Context {
            symbol,
            detail,
            confidence_min,
            root,
        }) => {
            let start = here(root);
            query_command(
                cli.json,
                |home| vex_app::graph::run_context(home, &start, &symbol, &detail, confidence_min),
                |out| print_answer(&out.answer, print_pretty),
            )
        }
        Some(Command::Impact {
            symbols,
            list,
            direction,
            depth,
            confidence_min,
            root,
        }) => {
            let direction = match direction.as_str() {
                "up" => "upstream".to_string(),
                "down" => "downstream".to_string(),
                _ => direction,
            };
            let req = vex_app::graph::ImpactRequest {
                list: list || symbols.len() > 1,
                roots: symbols,
                direction,
                depth,
                confidence_min,
            };
            let start = here(root);
            query_command(
                cli.json,
                |home| vex_app::graph::run_impact(home, &start, &req),
                |out| print_answer(&out.answer, print_pretty),
            )
        }
        Some(Command::Mcp) => cmd::mcp::run(),
        Some(Command::Daemon { action }) => match action {
            DaemonAction::Status => cmd::daemon::status(cli.json),
            DaemonAction::Start => cmd::daemon::start(cli.json),
            DaemonAction::Stop => cmd::daemon::stop(cli.json),
            DaemonAction::Restart => cmd::daemon::restart(cli.json),
            DaemonAction::Run { detach: true } => cmd::daemon::detach(),
            DaemonAction::Run { detach: false } => cmd::daemon::run(),
        },
        Some(Command::Projects) => project_command(cli.json, vex_app::tools::projects, |r| {
            for p in &r.projects {
                let gone = if p.present { "" } else { "  (missing)" };
                outln!("{}  {}  {}{gone}", p.project_id, p.name, p.root.display());
            }
        }),
        Some(Command::Changes {
            base,
            staged,
            max_depth,
            root,
        }) => {
            let req = vex_app::changes::Request {
                base,
                staged,
                max_depth,
            };
            let start = here(root);
            query_command(
                cli.json,
                |home| vex_app::changes::run(home, &start, &req),
                |out| print_pretty(&out.result),
            )
        }
        Some(Command::Map {
            focus,
            budget_tokens,
            root,
        }) => {
            let start = here(root);
            query_command(
                cli.json,
                |home| vex_app::map::run(home, &start, budget_tokens, &focus),
                |out| outln!("{}", out.text),
            )
        }
        Some(Command::Grep {
            pattern,
            path,
            literal,
            max_results,
            context_lines,
            root,
        }) => {
            let req = vex_app::grep::Request {
                pattern,
                path,
                literal,
                max_results,
                context_lines,
            };
            let start = here(root);
            query_command(
                cli.json,
                |home| vex_app::grep::run(home, &start, &req),
                print_grep,
            )
        }
        Some(Command::Parse { file, root }) => {
            let root = out::resolve(root);
            match vex_app::parse::parse(&root, &file) {
                Ok(out) => {
                    if cli.json {
                        out::write_out(&out.canonical_json());
                    } else {
                        let s = out.summary();
                        outln!(
                            "{} ({})",
                            s.file,
                            if s.language.is_empty() {
                                "no language"
                            } else {
                                &s.language
                            }
                        );
                        outln!(
                            "chunks: {}, symbols: {}, calls: {}, imports: {}",
                            s.chunks,
                            s.symbols,
                            s.calls,
                            s.imports
                        );
                    }
                    exit::OK
                }
                Err(e) => {
                    errln!("vex parse: {e}");
                    exit::USAGE
                }
            }
        }
        None => {
            errln!("usage: vex <command> [--json]   (vex --help for the list)");
            exit::USAGE
        }
    }
}

fn here(path: Option<std::path::PathBuf>) -> std::path::PathBuf {
    out::resolve(path)
}

fn print_index(r: &vex_app::index::IndexReport) {
    outln!(
        "{} files scanned: {} added, {} modified, {} removed; {} chunks; {} call edges, {} ref edges",
        r.files_scanned,
        r.added,
        r.modified,
        r.removed,
        r.chunks_ingested,
        r.call_edges,
        r.ref_edges
    );
    if let Some(e) = &r.embed {
        outln!(
            "vectors: {} embedded, {} pending ({}; cache {}h/{}m)",
            e.embedded,
            e.pending,
            e.model,
            e.cache_hits,
            e.cache_misses
        );
        if let Some(n) = &e.notice {
            errln!("vex: {n}");
        }
    }
}

/// Run a project command: resolve VEX_HOME, map errors to exit codes
/// (no index: 2; bad path: 1; anything else: 4), print the shared JSON
/// reply or the human form.
fn project_command<T: serde::Serialize>(
    json: bool,
    f: impl FnOnce(&std::path::Path) -> Result<T, vex_app::project::ProjectError>,
    human: impl FnOnce(&T),
) -> i32 {
    use vex_app::tools::{self, Reply};
    let Some(home) = vex_store_home() else {
        return emit(json, &Reply::no_home());
    };
    let result = f(&home);
    match &result {
        Ok(r) if !json => {
            human(r);
            exit::OK
        }
        _ => emit(json, &tools::reply_serde(&result)),
    }
}

/// Run a query command. With `--json` the shared reply (the same text the
/// MCP server returns) goes to stdout; otherwise `human` prints the
/// outcome. Errors go to stderr either way; the exit code follows the
/// reply's status (refused 1, no index 2, stale 3, could not run 4).
fn query_command<T: vex_app::tools::Reportable>(
    json: bool,
    run: impl FnOnce(&std::path::Path) -> Result<T, vex_app::project::ProjectError>,
    human: impl FnOnce(&T),
) -> i32 {
    use vex_app::tools::{self, Reply, Status};
    let Some(home) = vex_store_home() else {
        return emit(json, &Reply::no_home());
    };
    let result = run(&home);
    let reply = tools::reply(&result);
    match &result {
        Ok(out) if !json => {
            human(out);
            if reply.status == Status::Stale {
                stale_note();
            }
            exit_code(reply.status)
        }
        _ => emit(json, &reply),
    }
}

/// Print a reply: its text with `--json`, its error message on stderr.
fn emit(json: bool, reply: &vex_app::tools::Reply) -> i32 {
    if let Some((_, message)) = &reply.error {
        errln!("vex: {message}");
    }
    if json {
        outln!("{}", reply.text);
    }
    exit_code(reply.status)
}

fn exit_code(status: vex_app::tools::Status) -> i32 {
    use vex_app::tools::Status;
    match status {
        Status::Ok => exit::OK,
        Status::Refused => exit::USAGE,
        Status::NoIndex => exit::NO_INDEX,
        Status::Stale => exit::STALE,
        Status::Runtime => exit::RUNTIME,
    }
}

fn stale_note() {
    errln!("vex: the index is behind the working tree; run `vex index` to refresh");
}

/// The human form of a query answer that is a payload, plain text or a
/// refusal.
fn print_answer(answer: &vex_app::search::Answer, payload: impl FnOnce(&vex_app::pyjson::Json)) {
    use vex_app::search::Answer;
    match answer {
        Answer::Payload(p) => payload(p),
        Answer::Text(t) => outln!("{t}"),
        Answer::Error { message, .. } => errln!("vex: {message}"),
    }
}

fn print_pretty(doc: &vex_app::pyjson::Json) {
    let style = vex_app::pyjson::Style {
        ensure_ascii: false,
        compact: false,
    };
    outln!("{}", vex_app::pyjson::dumps(doc, style));
}

fn print_expand(out: &vex_app::expand::Outcome) {
    use vex_app::pyjson::Json;
    let s = |v: &Json, k: &str| v.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let print_source = |item: &Json| {
        let head = if item.get("symbol").is_some() {
            s(item, "symbol")
        } else {
            s(item, "chunk")
        };
        outln!("{head}  {}:{}", s(item, "file"), s(item, "lines"));
        outln!("{}\n", s(item, "source"));
    };
    print_answer(&out.answer, |p| match p.get("sources") {
        Some(Json::Arr(sources)) => {
            sources.iter().for_each(print_source);
            if let Some(Json::Arr(errors)) = p.get("errors") {
                for e in errors {
                    errln!("vex: {}: {}", s(e, "target"), s(e, "message"));
                }
            }
        }
        _ => print_source(p),
    });
}

fn print_grep(out: &vex_app::grep::Outcome) {
    use vex_app::pyjson::Json;
    if let Some(Json::Str(message)) = out.payload.get("error") {
        errln!("vex: {message}");
        return;
    }
    let s = |v: &Json, k: &str| v.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let mut last_owner: Option<&Option<String>> = None;
    if let Some(Json::Arr(hits)) = out.payload.get("hits") {
        for (h, owner) in hits.iter().zip(&out.owners) {
            if last_owner != Some(owner) {
                outln!("{}", owner.as_deref().unwrap_or("(no symbol)"));
                last_owner = Some(owner);
            }
            outln!("  {}:{}: {}", s(h, "file"), s(h, "line"), s(h, "text"));
        }
    }
    if let Some(Json::Str(hint)) = out.payload.get("recovery").and_then(|r| r.get("hint")) {
        errln!("vex: {hint}");
    }
}

fn print_search(p: &vex_app::pyjson::Json) {
    use vex_app::pyjson::Json;
    let s = |v: Option<&Json>| v.and_then(Json::as_str).unwrap_or("").to_string();
    if let Some(Json::Arr(results)) = p.get("results") {
        for r in results {
            let score = match r.get("score") {
                Some(Json::Float(f)) => format!("{f:.3}"),
                _ => String::new(),
            };
            outln!("{}:{}  {score}", s(r.get("file")), s(r.get("lines")));
            if let Some(first) = s(r.get("content")).lines().find(|l| !l.trim().is_empty()) {
                outln!("    {}", first.trim());
            }
        }
    } else if let Some(Json::Arr(leads)) = p.get("leads") {
        for l in leads {
            outln!(
                "{}  {}:{}",
                s(l.get("target")),
                s(l.get("file")),
                s(l.get("lines"))
            );
        }
        if let Some(a) = p.get("next_action").and_then(Json::as_str) {
            outln!("next: {a}");
        }
    }
}

fn vex_store_home() -> Option<std::path::PathBuf> {
    vex_app::home()
}

fn print_json<T: serde::Serialize>(value: &T) {
    let v: Value = serde_json::to_value(value).expect("results serialise");
    outln!("{v}");
}
