//! PeakBot entry point

use anyhow::Result;
use clap::{Parser, Subcommand};
use peakbot::{
    Config, FileStorage, ShellKind, SubAgentRegistry, Ui, get_config_file_path,
    install::{
        InstallAction, InstallOutcome, PathState, ServiceOp, ServicePlan, install_binary,
        install_target, path_state, resolve_token, service_op, web_token_path, write_web_token,
    },
    load_default_skills, load_mcp_servers, print_no_shell_warning,
};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Web,
    Tui,
    Stdio,
}

/// The two installer verbs (plan §E.12). `args_conflicts_with_subcommands = true`
/// on `Cli` rejects `peakbot --tui install` at the clap boundary — the only way
/// to make an "install vs. web UI" decision impossible is to make clap refuse to
/// parse it.
#[derive(Debug, Clone, Subcommand)]
enum Command {
    /// Copy this binary into your per-user application directory.
    Install,
    /// Manage the start-at-login service for this user.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

/// Sub-verbs of `peakbot service …`. `Install` carries its own `--bind` /
/// `--token` so the service install is a self-contained command; the loopback /
/// token invariant is enforced in [`ServicePlan::new`], not in clap.
#[derive(Debug, Clone, Subcommand)]
enum ServiceAction {
    /// Register (or re-register) the service. Idempotent.
    Install {
        #[arg(long, value_name = "ADDR")]
        bind: Option<std::net::SocketAddr>,
        #[arg(long, value_name = "SECRET")]
        token: Option<String>,
    },
    /// Remove the service. Leaves the binary and the token file alone.
    Uninstall,
    /// Print what is registered, where it points, and whether it is running.
    Status,
}

#[derive(Parser)]
#[command(name = "peakbot")]
#[command(about = "PeakBot — AI coding assistant")]
#[command(version)]
// §E.12 hard rule: a subcommand and a conflicting top-level flag cannot
// coexist. `peakbot --tui install` is a clap error, not a coin flip.
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// Run the terminal UI instead of the default web UI.
    #[arg(long, conflicts_with_all = ["stdio", "web"])]
    tui: bool,

    /// Run the NDJSON stdin/stdout frontend instead of the web UI
    /// (for IDE integrations). stdout becomes the protocol channel, so logs
    /// go to stderr.
    #[arg(long, conflicts_with = "web")]
    stdio: bool,

    /// Deprecated compatibility alias; the web UI is now the default.
    #[arg(long, hide = true, conflicts_with = "tui")]
    web: bool,

    /// Address the web server listens on (`host:port`). Defaults to loopback
    /// (`127.0.0.1:7823`). Binding beyond loopback requires
    /// `--token`/`PEAKBOT_WEB_TOKEN` — an unauthenticated agent must never
    /// be exposed to the network.
    #[arg(long, value_name = "ADDR", conflicts_with_all = ["tui", "stdio"])]
    bind: Option<std::net::SocketAddr>,

    /// Shared secret guarding the web server. When set, every request
    /// (assets, `/commands`, `/ws`) must present it via `?token=…` (first
    /// load, which then sets a cookie) or the `peakbot_token` cookie.
    /// Falls back to the `PEAKBOT_WEB_TOKEN` env var (preferred — keeps the
    /// secret out of shell history and `ps`) and to `<config_dir>/web-token`
    /// (track I, plan §E.5) so `peakbot service install --token …` is enough
    /// to start a token-guarded server without ever exporting the secret.
    #[arg(long, value_name = "SECRET", conflicts_with_all = ["tui", "stdio"])]
    token: Option<String>,

    /// Serve the web UI over HTTPS using PeakBot's built-in CA. On first use
    /// PeakBot self-signs a CA (in the OS cache dir) and prints the URL to
    /// install it on your phone; every boot mints a fresh leaf whose SANs follow
    /// this machine's addresses. Overrides `web.tls` in config.
    #[arg(long, conflicts_with_all = ["tui", "stdio"])]
    tls: bool,

    /// Add an extra name (DNS or IP) to the HTTPS certificate, repeatable.
    /// The leaf already covers loopback, this machine's LAN IP, and its mDNS
    /// `<hostname>.local` name automatically; use this for any additional host
    /// a client might dial (e.g. `--tls-name peakbot.lan --tls-name 10.0.0.9`).
    /// Only meaningful with `--tls`.
    #[arg(long = "tls-name", value_name = "NAME", requires = "tls")]
    tls_name: Vec<String>,

    /// Subcommand — `install` (copy binary) or `service …` (manage the login
    /// service). Absent ⇒ run the configured UI. Subcommands dispatch
    /// **before** `Config::load()` — `peakbot install` works on a machine
    /// with no config, which is the entire point of the verb.
    #[command(subcommand)]
    command: Option<Command>,
}

impl Cli {
    fn mode(&self) -> Mode {
        if self.tui {
            Mode::Tui
        } else if self.stdio {
            Mode::Stdio
        } else {
            Mode::Web
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstRun {
    Proceed,
    SetupWizard,
    Refuse,
}

fn first_run_action(config_found: bool, has_key: bool, mode: Mode, windowed: bool) -> FirstRun {
    if config_found || has_key {
        FirstRun::Proceed
    } else if mode == Mode::Web && windowed {
        FirstRun::SetupWizard
    } else {
        FirstRun::Refuse
    }
}

/// Check if the provider has an API key configured.
/// Returns true if any API key is set (OpenRouter, OpenAI, or LlamaCpp).
fn has_api_key(config: &Config) -> bool {
    match &config.provider {
        peakbot::ProviderConfig::OpenRouter(c) => c.api_key.is_some(),
        peakbot::ProviderConfig::OpenAI(c) => c.api_key.is_some(),
        peakbot::ProviderConfig::Anthropic(_) => true, // Anthropic key optional (local servers)
        peakbot::ProviderConfig::LlamaCpp(c) => c.api_key.is_some(),
        peakbot::ProviderConfig::Ollama(_) => true, // Ollama uses no API key
    }
}

/// Print a friendly "config not found" error with instructions.
fn print_config_not_found_message(config_path: &std::path::Path) {
    eprintln!();
    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║                    ⚠️  Config not found!                        ║");
    eprintln!("╚══════════════════════════════════════════════════════════════╝");
    eprintln!();
    eprintln!("I couldn't find my config file at:");
    eprintln!();
    eprintln!("  {}", config_path.display());
    eprintln!();
    eprintln!("Create this file with content like:");
    eprintln!();
    eprintln!("  providers:");
    eprintln!("    - name: openrouter");
    eprintln!("      type: openrouter");
    eprintln!("      api_key: sk-or-v1-your-key-here");
    eprintln!("      models:");
    eprintln!("        - name: anthropic/claude-3.7-sonnet");
    eprintln!("          alias: sonnet");
    eprintln!();
    eprintln!("  default_model: sonnet");
    eprintln!();
    eprintln!("Find models at: https://openrouter.ai/models");
    eprintln!();
    eprintln!("For local models, use Ollama:");
    eprintln!();
    eprintln!("  providers:");
    eprintln!("    - name: ollama");
    eprintln!("      type: ollama");
    eprintln!("      base_url: http://localhost:11434");
    eprintln!("      models:");
    eprintln!("        - name: llama3");
    eprintln!("          alias: local");
    eprintln!();
    eprintln!("  default_model: local");
    eprintln!();
    eprintln!("No graphical session detected (no DISPLAY/WAYLAND_DISPLAY, or you are over SSH),");
    eprintln!("so the setup wizard cannot open a browser here.");
    eprintln!("Write the file above by hand, then run `peakbot` again.");
    eprintln!(
        "On a desktop, bare `peakbot` opens the setup wizard; a TUI configurator is future work."
    );
    eprintln!();
}

/// Read the web-token file for the boot token chain. **Best-effort** —
/// missing file, whitespace-only content, or unreadable-permissions all
/// collapse to `None` so the chain still works on a fresh box. We never
/// want a token-file glitch to block a default (tokenless) loopback boot.
fn read_web_token_for_chain(path: &std::path::Path) -> Option<String> {
    match peakbot::install::read_web_token(path) {
        Ok(t) => t,
        Err(e) => {
            // Permission denied on the token file is a *configuration*
            // mistake, not a reason to fail boot. Log via tracing and
            // move on — the user will see the 401 and fix the perms.
            eprintln!(
                "warning: could not read web-token file ({}): {e}",
                path.display()
            );
            None
        }
    }
}

/// Resolve the install target for the CLI. Mirrors
/// [`peakbot::install::install_target`] but turns the `None` case into a
/// one-line human error (plan §E.14 #1).
fn resolve_install_target() -> anyhow::Result<std::path::PathBuf> {
    install_target().ok_or_else(|| {
        anyhow::anyhow!("cannot determine install target — no home / data-local dir")
    })
}

/// Print the result of `peakbot install` per §E.12: target, action, the
/// PATH verdict, and the notes. CLI and wizard say the same words
/// because both read the same `notes: Vec<String>` from the core.
fn print_install_result(outcome: &InstallOutcome) {
    println!("Target: {}", outcome.target.display());
    println!("Source: {}", outcome.source.display());
    let action = match outcome.action {
        InstallAction::AlreadyCurrent => "already_current",
        InstallAction::Installed => "installed",
        InstallAction::Replaced => "replaced",
    };
    println!("Action: {action}");

    // PATH verdict uses the *process* PATH. We're at the boundary, so
    // reading `PATH` here is fine — and the only place the install CLI
    // is allowed to touch the env.
    let path_var: std::ffi::OsString = std::env::var_os("PATH").unwrap_or_default();
    match path_state(&path_var, &outcome.target) {
        PathState::OnPath => println!("PATH: on PATH"),
        PathState::Shadowed { by } => println!("PATH: shadowed by {}", by.display()),
        PathState::NotOnPath { hint } => println!("PATH: not on PATH — {hint}"),
    }
    if !outcome.notes.is_empty() {
        println!("Notes:");
        for n in &outcome.notes {
            println!("  - {n}");
        }
    }
}

/// Choose the path the service should run (plan §E.5 "installed target if it
/// exists, else `current_exe()`"). Returns the path and an optional note
/// describing the fallback so the caller (CLI or HTTP handler) can attach
/// the same wording to its `notes` array. Pure: only filesystem stat of the
/// install target + `current_exe()` resolution (no subprocesses, no env
/// mutation), so a unit test exercises both branches without a real service.
fn resolve_service_exe() -> (std::path::PathBuf, Option<String>) {
    let target = resolve_install_target().ok();
    let Some(target) = target else {
        // No install target is reachable at all — fall back to the
        // running binary and tell the user the install path is broken too.
        let here = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("peakbot"));
        return (
            here.clone(),
            Some(format!(
                "the service points at {}, which is where you ran PeakBot from — \
                 run `peakbot install` so it survives a move (no install target \
                 could be resolved on this machine)",
                here.display()
            )),
        );
    };
    if std::fs::symlink_metadata(&target).is_ok() {
        return (target, None);
    }
    // Fallback: target dir doesn't exist yet, so the install copy isn't
    // there either. Point at the running binary so the service still
    // works on a first run; the wizard/CLI surfaces the recommendation.
    let here = std::env::current_exe().unwrap_or_else(|_| target.clone());
    (
        here.clone(),
        Some(format!(
            "the service points at {}, which is where you ran PeakBot from — \
             run `peakbot install` so it survives a move",
            here.display()
        )),
    )
}

/// Build a `ServicePlan` from CLI `service install` arguments. The bind
/// default is `127.0.0.1:7823` (the same as the web UI). Returns
/// `PlanError` (the loopback/token invariant) wrapped as `anyhow::Error`
/// so the caller can `?` through a single error type. Clap can't enforce
/// the invariant because it doesn't know about the bind/token relationship.
fn build_plan_for_install(
    bind: Option<std::net::SocketAddr>,
    token: Option<&str>,
) -> Result<(ServicePlan, Option<String>), anyhow::Error> {
    let bind = bind.unwrap_or_else(|| {
        peakbot::ui::DEFAULT_WEB_ADDR
            .parse()
            .expect("DEFAULT_WEB_ADDR is a valid SocketAddr literal")
    });
    // If the user didn't pass --token, fall back to the web-token file —
    // the same chain main uses for a live bind. The artifact never
    // embeds the secret; the binary reads it back at launch.
    let owned_token: Option<String> = match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => Some(t.to_string()),
        None => web_token_path()
            .as_deref()
            .and_then(|p| peakbot::install::read_web_token(p).ok().flatten()),
    };
    let (exe, fallback_note) = resolve_service_exe();
    Ok((ServicePlan::new(exe, bind, owned_token)?, fallback_note))
}

/// Dispatch a subcommand. Returns `Ok(())` after printing the result;
/// errors are propagated as `Err` so the caller can exit non-zero.
fn run_subcommand(cmd: Command, cli: &Cli) -> Result<()> {
    match cmd {
        Command::Install => {
            let outcome = install_binary().map_err(|e| anyhow::anyhow!("install failed: {e}"))?;
            print_install_result(&outcome);
        }
        Command::Service { action } => match action {
            ServiceAction::Install { bind, token } => {
                let (plan, fallback_note) = build_plan_for_install(bind, token.as_deref())
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                // If the caller passed --token explicitly, write the
                // web-token file as a side-effect so a later
                // `peakbot` (web) boot finds it via the chain. The
                // service artifact itself never embeds the secret.
                if let Some(t) = token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                    let path = web_token_path()
                        .ok_or_else(|| anyhow::anyhow!("no config dir for web-token file"))?;
                    write_web_token(&path, t)
                        .map_err(|e| anyhow::anyhow!("failed to write web-token: {e}"))?;
                    println!("Wrote web-token to: {}", path.display());
                }
                let mut report = service_op(ServiceOp::Install { plan })
                    .map_err(|e| anyhow::anyhow!("service install failed: {e}"))?;
                // Surface the §E.5 fallback note when the service is
                // pointing at the running binary rather than the install
                // target. The note is produced by the plan builder, not
                // by `service_op`, so the wording stays identical to the
                // wizard's.
                if let Some(note) = fallback_note {
                    report.notes.push(note);
                }
                println!("Service installed.");
                print_service_report(&report);
            }
            ServiceAction::Uninstall => {
                let mut report = service_op(ServiceOp::Uninstall)
                    .map_err(|e| anyhow::anyhow!("service uninstall failed: {e}"))?;
                // §E.5: uninstall does NOT delete the token file. Carry
                // the same note the wizard does so the CLI and the
                // wizard say exactly the same words (§E.12).
                if let Some(path) = web_token_path() {
                    report.notes.push(format!(
                        "the web-token file at {} was NOT deleted; \
                         remove it by hand if you want to drop the secret.",
                        path.display()
                    ));
                }
                println!("Service uninstalled.");
                print_service_report(&report);
            }
            ServiceAction::Status => {
                let report = service_op(ServiceOp::Status)
                    .map_err(|e| anyhow::anyhow!("service status failed: {e}"))?;
                print_service_report(&report);
            }
        },
    }
    let _ = cli; // cli is unused here today; kept so future verb args can pull from it.
    Ok(())
}

/// Print a [`ServiceReport`]. One printer for all three verbs so the CLI
/// and the wizard say the same words about the same fields (§E.12).
fn print_service_report(report: &peakbot::install::ServiceReport) {
    println!("Manager:   {}", report.manager.as_wire());
    println!("Name:      {}", report.name);
    if let Some(artifact) = &report.artifact {
        println!("Artifact:  {}", artifact.display());
    }
    println!("Installed: {}", report.installed);
    if let Some(exe) = &report.exe {
        println!("Exec:      {}", exe.display());
    }
    println!("State:     {}", report.run_state.as_wire());
    println!("Survives logout: {}", report.survives_logout);
    if !report.commands.is_empty() {
        println!("Commands:");
        for c in &report.commands {
            println!("  $ {c}");
        }
    }
    if !report.notes.is_empty() {
        println!("Notes:");
        for n in &report.notes {
            println!("  - {n}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Mode must be known before tracing init: under stdio stdout is the
    // NDJSON wire, so logs MUST go to stderr or they corrupt the protocol.
    let cli = Cli::parse();
    let mode = cli.mode();
    if cli.web {
        eprintln!("warning: --web is deprecated; the web UI is now the default");
    }

    // ── Subcommand dispatch — BEFORE `Config::load()` (plan §E.12) ──
    //
    // `peakbot install` and `peakbot service …` must work on a machine with
    // no config — the install verb is the *first* step on a fresh box, and
    // the service verb is what gets re-run after every self-update. Any
    // "no config" gate that ran before this branch would be a bug.
    if let Some(cmd) = cli.command.clone() {
        return run_subcommand(cmd, &cli);
    }

    let subscriber = tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env());
    if mode == Mode::Stdio {
        subscriber.with_writer(std::io::stderr).init();
    } else {
        subscriber.init();
    }

    // ── Shared setup ──────────────────────────────────────────────

    // Load configuration with metadata about what was found
    let loaded = Config::load()?;
    let config_found = loaded.config_file_found;
    let mut needs_setup = false;

    match first_run_action(
        config_found,
        has_api_key(&loaded.config),
        mode,
        peakbot::ui::web::windowed_session(),
    ) {
        FirstRun::Proceed => {}
        FirstRun::SetupWizard => {
            needs_setup = true;
            eprintln!("No config found — opening the setup wizard…");
        }
        FirstRun::Refuse => {
            if let Some(config_path) = loaded.config_file_path.as_deref() {
                print_config_not_found_message(config_path);
            } else if let Some(config_path) = get_config_file_path() {
                print_config_not_found_message(&config_path);
            }
            anyhow::bail!("No config file found. See instructions above.");
        }
    }

    let mut config = loaded.config;
    // Boot-only: every HTTP client built from here on inherits these.
    peakbot::http::init_timeouts(config.http.clone());
    // Load skills relative to the boot cwd (an allowed mint-site read of the
    // process cwd). Warnings are surfaced later as system messages by the
    // session factory; per-session verbs re-scan against the session cwd.
    let boot_cwd = std::env::current_dir().unwrap_or_default();
    let (skills, skill_warnings) = load_default_skills(&boot_cwd);
    for w in &skill_warnings {
        tracing::warn!("{w}");
    }
    let skills_count = skills.len(); // Keep count before moving skills

    // Detect the shell first — the system prompt needs it so the model is
    // told which syntax + shell tool to use (#82). On Windows with no shell
    // found, warn and continue; other tools still work.
    let shell_kind = ShellKind::detect();

    // Build the model registry. Two paths:
    // - `providers:` list declared → multi-model, `/model` enabled.
    // - Legacy `provider:` block → synthesised one-entry registry
    //   with alias `default`. `/model default` is a no-op; the user
    //   gets a "no other models declared" message if they try to
    //   switch. See `multi-model.md`.
    let model_registry = match config.build_model_registry() {
        Ok(reg) => Arc::new(reg),
        Err(e) => {
            anyhow::bail!("Invalid model configuration: {e}");
        }
    };

    // Load MCP servers
    let mcp_handles = load_mcp_servers(&config).await?;
    let mcp_tools_count = mcp_handles.len(); // Keep count before moving handles
    // Wrap the handles in an Arc so each session can re-derive its tools
    // list without restarting the underlying subprocesses. McpTool: Clone
    // (rig 0.33) makes the per-session tool list cheap. See session.rs.
    let mcp_handles_arc = Arc::new(mcp_handles);

    let searxng_config = config
        .searxng_enabled()
        .then_some(config.searxng.clone())
        .flatten();

    // Create shared conversation storage if enabled. One storage instance
    // is shared by every session — it writes distinct files per
    // conversation id (see session::SessionDeps).
    let storage: Option<Arc<dyn peakbot::ConversationStorage>> = if config.conversation_enabled() {
        let storage_dir = config.conversation_storage_dir();
        match FileStorage::new(storage_dir.clone()) {
            Ok(storage) => {
                tracing::info!("Conversation storage enabled at: {:?}", storage_dir);
                Some(Arc::new(storage))
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to create conversation storage at {:?}: {}. \
                     Continuing without persistence.",
                    storage_dir,
                    e
                );
                None
            }
        }
    } else {
        None
    };

    // Pipeline. Shared across sessions via Arc. Roles resolve their model
    // alias against the model registry at construction (unknown alias →
    // clear boot error).
    let pipeline_registry = if config.pipeline_enabled() {
        let pipeline_config = config.pipeline().unwrap();
        match SubAgentRegistry::new(pipeline_config, &model_registry, &skills.names()) {
            Ok(reg) => Some(Arc::new(reg)),
            Err(e) => anyhow::bail!("Invalid pipeline configuration: {e}"),
        }
    } else {
        None
    };

    // Mirror the registry's boot provider into `config.provider` so
    // `AgentRunner::new`'s compaction-model construction reads the active
    // provider, not the stale `OpenRouterConfig::default()` (api_key=None)
    // that would fail with a misleading "OpenRouter API key not configured".
    // Matches the `/model` switch path at `lib.rs:1082`. The return value is
    // unused — each session derives its own provider from the resolved boot
    // model; only the `config.provider` side-effect matters here.
    config.resolve_and_mirror_boot_provider(&model_registry);

    // Log the detected shell here; each session applies it to its own
    // StateManager via the factory. On Windows with no shell, warn once.
    if let Some(ref sk) = shell_kind {
        tracing::info!("Detected shell: {} ({})", sk.name(), sk.executable());
    } else {
        print_no_shell_warning();
    }

    // Build the shared vector store (doc_index / doc_search) when configured
    // and enabled. Opened once here and injected into both tools via the
    // provider builder; reused across `/model` switches via RebuildContext.
    // A failure to open is non-fatal — we warn and continue without the tools,
    // matching how a missing shell degrades gracefully.
    let vector_store = match config.vector_db.as_ref() {
        Some(vc) if vc.enabled => match peakbot::vector::VectorStore::open(vc) {
            Ok(store) => {
                tracing::info!(
                    "Vector store enabled; DB resolved per session cwd from: {}",
                    vc.db_path
                );

                Some(store)
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to open vector store at {}: {e}. \
                     Continuing without doc_index/doc_search.",
                    vc.db_path
                );
                None
            }
        },
        _ => None,
    };

    // Build the shared, immutable session deps once. Each session (the
    // single TUI/stdio session, or one per web-socket connection) is built
    // from these via `create_session`. `Arc` so the web UI can hand a clone
    // to every connection handler. See `src/session.rs`.
    let session_deps = Arc::new(peakbot::SessionDeps {
        config: config.clone(),
        model_registry: model_registry.clone(),
        skills,
        skill_warnings,
        mcp_handles: mcp_handles_arc.clone(),
        searxng_config,
        pipeline_registry,
        vector_store,
        shell_kind,
        storage,
        mcp_tools_count,
        skills_count,
    });

    use peakbot::ui::{ReplUi, StdioUi, WebUi, build_models_snapshot};

    // The web UI builds one session per WebSocket connection, so it takes
    // the deps and never boots a single shared session. The TUI and stdio
    // views are single-session: build one, drive it, drop it (teardown).
    if mode == Mode::Web {
        // Resolve the bind address (default loopback) and the shared secret
        // (flag > env > web-token file). Parse-at-the-boundary: validate
        // here, then trust the Option<token> inside WebUi.
        let addr: std::net::SocketAddr = cli.bind.unwrap_or_else(|| {
            peakbot::ui::DEFAULT_WEB_ADDR
                .parse()
                .expect("DEFAULT_WEB_ADDR is a valid SocketAddr literal")
        });
        // §E.5 chain: --token > $PEAKBOT_WEB_TOKEN > <config_dir>/web-token.
        // The file is written by `peakbot service install --token …` and is
        // the only artifact that *can* carry the secret; the unit/plist/task
        // never do. `resolve_token` collapses empty/whitespace to "absent".
        let env_token = std::env::var("PEAKBOT_WEB_TOKEN").ok();
        let file_token = web_token_path()
            .as_deref()
            .and_then(read_web_token_for_chain);
        let token = resolve_token(
            cli.token.as_deref(),
            env_token.as_deref(),
            file_token.as_deref(),
        );

        // An agent that can run shell commands must never be reachable on a
        // non-loopback address without a secret. Make the footgun impossible.
        if !addr.ip().is_loopback() && token.is_none() {
            anyhow::bail!(
                "refusing to bind the web UI to non-loopback {addr} without a token — \
                 set --token, PEAKBOT_WEB_TOKEN, or write <config_dir>/web-token"
            );
        }

        let active_alias = model_registry.default_alias().to_string();
        // Flag overrides config, same precedent as the token resolution above.
        let tls = cli.tls || session_deps.config.web.tls;
        let mut ui = WebUi::new(
            addr,
            session_deps.clone(),
            build_models_snapshot(&model_registry),
            active_alias,
            token,
            tls,
            cli.tls_name.clone(),
            needs_setup,
        );
        ui.init().await?;
        ui.run().await?;
        ui.shutdown().await?;
        return Ok(());
    }

    // Single session for the TUI / stdio Views.
    let session = peakbot::create_session(&session_deps, None)?;
    let state_manager = session.state_manager.clone();
    let action_sender = session.action_sender.clone();
    let boot_alias = session.model_alias.clone();

    // Two single-session Views share the Model/Controller seam: `--stdio`
    // is the NDJSON frontend, default is the REPL.
    if mode == Mode::Stdio {
        let mut ui = StdioUi::new(
            state_manager.clone(),
            action_sender,
            build_models_snapshot(&model_registry),
            boot_alias.clone(),
        );
        ui.init().await?;
        ui.run().await?;
        ui.shutdown().await?;
    } else {
        // The registry lets the View intercept/validate `/model <alias>`
        // before any UiAction is dispatched.
        let mut ui =
            ReplUi::new_with_registry(state_manager.clone(), action_sender, model_registry.clone());
        ui.init().await?;
        ui.run().await?;
        ui.shutdown().await?;
    }

    // Dropping the session drops its `action_sender`, which unwinds the
    // controller's event loop, aborts the agent loop, and kills any bg
    // PTY children — the clean teardown path (see session::Session).
    drop(session);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("peakbot").chain(args.iter().copied()))
    }

    #[test]
    fn cli_defaults_to_web_mode() {
        assert_eq!(parse(&[]).unwrap().mode(), Mode::Web);
    }

    #[test]
    fn cli_tui_flag_selects_tui() {
        assert_eq!(parse(&["--tui"]).unwrap().mode(), Mode::Tui);
    }

    #[test]
    fn cli_stdio_flag_selects_stdio() {
        assert_eq!(parse(&["--stdio"]).unwrap().mode(), Mode::Stdio);
    }

    #[test]
    fn cli_deprecated_web_alias_selects_web() {
        assert_eq!(parse(&["--web"]).unwrap().mode(), Mode::Web);
    }

    #[test]
    fn cli_rejects_conflicting_frontends() {
        assert!(parse(&["--tui", "--stdio"]).is_err());
        assert!(parse(&["--web", "--tui"]).is_err());
    }

    #[test]
    fn cli_rejects_web_options_with_tui() {
        assert!(parse(&["--tui", "--bind", "1.2.3.4:1"]).is_err());
    }

    #[test]
    fn cli_accepts_bind_without_web_alias() {
        assert_eq!(parse(&["--bind", "127.0.0.1:1"]).unwrap().mode(), Mode::Web);
    }

    // ── Subcommand grammar (plan §E.12). ──────────────────────────

    #[test]
    fn cli_install_subcommand_parses() {
        let cli = parse(&["install"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Install)));
    }

    #[test]
    fn cli_no_subcommand_means_no_command() {
        let cli = parse(&[]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn cli_subcommand_conflicts_with_tui() {
        // args_conflicts_with_subcommands = true ⇒ the clap boundary
        // refuses to parse "install" alongside a frontend flag.
        assert!(parse(&["--tui", "install"]).is_err());
        assert!(parse(&["--stdio", "install"]).is_err());
    }

    #[test]
    fn cli_subcommand_conflicts_with_web_bind_token() {
        // The web-mode flags are equally incompatible with a subcommand.
        assert!(parse(&["install", "--bind", "1.2.3.4:1"]).is_err());
        assert!(parse(&["--bind", "127.0.0.1:1", "install"]).is_err());
        assert!(parse(&["--token", "x", "install"]).is_err());
    }

    #[test]
    fn cli_service_install_parses_bind_and_token() {
        let cli = parse(&[
            "service",
            "install",
            "--bind",
            "127.0.0.1:7823",
            "--token",
            "abc",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Service {
                action: ServiceAction::Install { bind, token },
            }) => {
                assert_eq!(bind.unwrap().port(), 7823);
                assert_eq!(token.as_deref(), Some("abc"));
            }
            other => panic!("expected Service::Install, got {other:?}"),
        }
    }

    #[test]
    fn cli_service_install_accepts_no_args() {
        // `service install` with no bind/token is valid at the parse
        // boundary — the loopback/token invariant is enforced later
        // in ServicePlan::new.
        let cli = parse(&["service", "install"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Install {
                    bind: None,
                    token: None
                }
            })
        ));
    }

    #[test]
    fn cli_service_install_with_lan_bind_parses_but_must_fail_in_plan() {
        // Parses fine here; the *plan* layer is what enforces the rule.
        let cli = parse(&[
            "service",
            "install",
            "--bind",
            "0.0.0.0:7823",
            "--token",
            "real",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Install { .. }
            })
        ));
    }

    #[test]
    fn cli_service_uninstall_parses() {
        let cli = parse(&["service", "uninstall"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Uninstall
            })
        ));
    }

    #[test]
    fn cli_service_status_parses() {
        let cli = parse(&["service", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Status
            })
        ));
    }

    #[test]
    fn cli_service_alone_is_an_error() {
        // `service` without a sub-action is not a complete command —
        // clap rejects it.
        assert!(parse(&["service"]).is_err());
    }

    #[test]
    fn cli_service_install_bad_bind_is_rejected_by_clap() {
        // SocketAddr parsing is clap's job; an unparseable value never
        // reaches the plan layer.
        assert!(parse(&["service", "install", "--bind", "not-an-addr"]).is_err());
    }

    #[test]
    fn first_run_proceeds_with_config_or_key() {
        for mode in [Mode::Web, Mode::Tui, Mode::Stdio] {
            for windowed in [false, true] {
                assert_eq!(
                    first_run_action(true, false, mode, windowed),
                    FirstRun::Proceed
                );
                assert_eq!(
                    first_run_action(false, true, mode, windowed),
                    FirstRun::Proceed
                );
            }
        }
    }

    #[test]
    fn first_run_opens_wizard_only_for_windowed_web() {
        assert_eq!(
            first_run_action(false, false, Mode::Web, true),
            FirstRun::SetupWizard
        );
        assert_eq!(
            first_run_action(false, false, Mode::Web, false),
            FirstRun::Refuse
        );
        assert_eq!(
            first_run_action(false, false, Mode::Tui, true),
            FirstRun::Refuse
        );
        assert_eq!(
            first_run_action(false, false, Mode::Stdio, true),
            FirstRun::Refuse
        );
    }

    // ── §E.5 — `resolve_service_exe` picks installed target when present,
    // else falls back to `current_exe()` with an explanatory note. ──

    #[test]
    fn resolve_service_exe_falls_back_to_current_exe_with_note_when_target_absent() {
        // CI never installs peakbot on CI, so the install target is
        // absent and the fallback branch fires. The returned path must
        // be the running test binary and the note must be present +
        // readable.
        //
        // **Skip on a developer machine that has peakbot installed.**
        // A developer who has run `peakbot install` has a real binary
        // at `~/.local/bin/peakbot`; the function (correctly) returns
        // it, and the sibling test below owns that branch.
        let target = install_target().expect("a host with a home dir has an install target");
        if std::fs::symlink_metadata(&target).is_ok() {
            eprintln!(
                "skipping: install target {} already exists; \
                 the sibling test covers the installed-target branch",
                target.display()
            );
            return;
        }
        let (path, note) = resolve_service_exe();
        let here = std::env::current_exe().expect("test runner has a current_exe");
        assert_eq!(path, here, "fallback must equal current_exe()");
        let note = note.expect("fallback must surface a §E.5 note");
        assert!(
            note.contains("run `peakbot install`"),
            "note must point the user at the install verb; got: {note}"
        );
        assert!(
            note.contains(&here.display().to_string()),
            "note must name the path it is pointing at; got: {note}"
        );
    }

    #[test]
    fn resolve_service_exe_picks_installed_target_when_present_and_silences_note() {
        // Create a sentinel file at the install target so the function
        // sees it as "installed", then assert it is returned and the
        // note is dropped (a real install means there is nothing to
        // say).
        //
        // **Preserve whatever was at the install target before.**
        // `TempFileGuard` snapshots the existing bytes (and perms) on
        // construction and restores them on `Drop`, so a developer
        // running this on a machine that already has `peakbot`
        // installed doesn't lose their binary. The earlier version of
        // the guard just removed the file in `Drop` and silently
        // uninstalled PeakBot off the operator's PATH.
        let target = install_target().expect("a host with a home dir has an install target");
        let _guard = TempFileGuard::new(target.clone());
        std::fs::write(&target, b"sentinel").expect("write sentinel at install target");
        let (path, note) = resolve_service_exe();
        assert_eq!(path, target, "installed target must win over current_exe()");
        assert!(
            note.is_none(),
            "no fallback note when the target is present"
        );
    }

    /// RAII restore-on-drop for a file path the test swapped.
    /// Captures the original state (regular file contents, symlink
    /// so a developer's installed binary at `~/.local/bin/peakbot`
    /// is not silently uninstalled by this test.
    ///
    /// Handles dangling symlinks correctly: `symlink_metadata` +
    /// `read_link` detect the symlink entry regardless of whether
    /// the target exists, and `remove_file` clears it so the test
    /// can write a sentinel. On drop, the symlink is recreated.
    struct TempFileGuard {
        path: std::path::PathBuf,
        saved: Option<TempFileGuardSaved>,
    }

    enum TempFileGuardSaved {
        File(Vec<u8>, Option<std::fs::Permissions>),
        Symlink(std::path::PathBuf),
    }

    impl TempFileGuard {
        fn new(p: std::path::PathBuf) -> Self {
            let saved = match std::fs::symlink_metadata(&p) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    // Symlink (valid or dangling) — save the link target
                    let link_target = std::fs::read_link(&p).ok();
                    let _ = std::fs::remove_file(&p);
                    link_target.map(TempFileGuardSaved::Symlink)
                }
                Ok(_) => {
                    // Regular file — save contents and permissions
                    match std::fs::read(&p) {
                        Ok(bytes) => {
                            let perms = std::fs::metadata(&p).ok().map(|m| m.permissions());
                            let _ = std::fs::remove_file(&p);
                            Some(TempFileGuardSaved::File(bytes, perms))
                        }
                        Err(_) => {
                            let _ = std::fs::remove_file(&p);
                            None
                        }
                    }
                }
                Err(_) => None, // Path does not exist
            };
            Self { path: p, saved }
        }
    }

    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(TempFileGuardSaved::File(bytes, permissions)) => {
                    // Real file was here before the test — restore it
                    // verbatim (and its permissions) so the
                    // developer's install survives.
                    if let Err(e) = std::fs::write(&self.path, bytes) {
                        eprintln!("warning: failed to restore {}: {e}", self.path.display());
                    } else if let Some(perms) = permissions
                        && let Err(e) = std::fs::set_permissions(&self.path, perms.clone())
                    {
                        eprintln!(
                            "warning: failed to restore permissions on {}: {e}",
                            self.path.display()
                        );
                    }
                }
                Some(TempFileGuardSaved::Symlink(target)) => {
                    // Symlink was here before — recreate it
                    let _ = std::fs::remove_file(&self.path);
                    #[cfg(unix)]
                    if let Err(e) = std::os::unix::fs::symlink(target, &self.path) {
                        eprintln!(
                            "warning: failed to restore symlink {}: {e}",
                            self.path.display()
                        );
                    }
                    #[cfg(windows)]
                    if let Err(e) = std::os::windows::fs::symlink_file(target, &self.path) {
                        eprintln!(
                            "warning: failed to restore symlink {}: {e}",
                            self.path.display()
                        );
                    }
                }
                None => {
                    // No file or symlink before the test — the target
                    // was meant to be absent. Delete the sentinel we
                    // wrote so the fallback-branch test can run again
                    // next time.
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
    }
}
