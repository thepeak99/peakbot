//! PeakBot entry point

use anyhow::Result;
use clap::{Parser, Subcommand};
use peakbot::config::resolve::{LoadError, ParseCtx, Source};
use peakbot::config::{ConfigErrors, ConfigFileKind, auto_migrate, resolve_token};
use peakbot::{
    Config, FileStorage, RepoMode, ShellKind, Ui,
    install::{
        InstallAction, InstallOutcome, PathState, ServiceOp, ServicePlan, install_binary,
        install_target, path_state, service_op,
    },
    load_default_skills, load_mcp_servers, print_no_shell_warning,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::util::SubscriberInitExt;

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
        /// Run the service over HTTPS with the built-in CA (the same
        /// machinery as the foreground `--tls`).
        #[arg(long)]
        tls: bool,
        /// Add an extra name (DNS or IP) to the HTTPS certificate,
        /// repeatable. Only meaningful with `--tls`.
        #[arg(long = "tls-name", value_name = "NAME", requires = "tls")]
        tls_name: Vec<String>,
    },
    /// Remove the service. Leaves the binary and config.yaml alone.
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

    /// Address the web server listens on (`host:port`). Overrides
    /// `general.web.bind`; defaults to loopback (`127.0.0.1:7823`). Binding
    /// beyond loopback requires a token (`--token`, `PEAKBOT_WEB_TOKEN` or
    /// `general.web.token`) — an unauthenticated agent must never be exposed
    /// to the network.
    #[arg(long, value_name = "ADDR", conflicts_with_all = ["tui", "stdio"])]
    bind: Option<std::net::SocketAddr>,

    /// Shared secret guarding the web server. When set, every request
    /// (assets, `/commands`, `/ws`) must present it via `?token=…` (first
    /// load, which then sets a cookie) or the `peakbot_token` cookie.
    /// Falls back to the `PEAKBOT_WEB_TOKEN` env var (keeps the secret out of
    /// shell history and `ps`), then to `general.web.token` in config.yaml
    /// (which `peakbot service install --token …` writes).
    #[arg(long, value_name = "SECRET", conflicts_with_all = ["tui", "stdio"])]
    token: Option<String>,

    /// Serve the web UI over HTTPS using PeakBot's built-in CA. On first use
    /// PeakBot self-signs a CA (in the OS cache dir) and prints the URL to
    /// install it on your phone; every boot mints a fresh leaf whose SANs follow
    /// this machine's addresses. On when this flag or `general.web.tls` is set.
    #[arg(long, conflicts_with_all = ["tui", "stdio"])]
    tls: bool,

    /// Add an extra name (DNS or IP) to the HTTPS certificate, repeatable.
    /// The leaf already covers loopback, this machine's LAN IP, and its mDNS
    /// `<hostname>.local` name automatically; use this for any additional host
    /// a client might dial (e.g. `--tls-name peakbot.lan --tls-name 10.0.0.9`).
    /// Only meaningful with `--tls`.
    #[arg(long = "tls-name", value_name = "NAME", requires = "tls")]
    tls_name: Vec<String>,

    /// Pin a global profile; repo profiles are ignored. Every conversation
    /// runs under it and `/profile <name>` is refused. Applies to `--tui`,
    /// `--stdio`, and the web UI alike.
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,

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

/// Print a friendly "config not found" error with instructions.
fn print_config_not_found_message(config_path: &Path) {
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
    eprintln!("  profiles:");
    eprintln!("    default:");
    eprintln!("      agents:");
    eprintln!("        main:");
    eprintln!("          model: sonnet");
    eprintln!();
    eprintln!("Find models at: https://openrouter.ai/models");
    eprintln!("For local models, use an `ollama` provider (base_url: http://localhost:11434).");
    eprintln!();
    eprintln!("Write the file above by hand, then run `peakbot` again.");
    eprintln!();
}

/// The text boot prints, after the migration error itself, when step 0
/// fails (spec §6.9).
const CANNOT_START_WITH_FILE: &str = "PeakBot cannot start with this file. Fix it and restart PeakBot, \
     or keep using PeakBot 0.23 with it.";

/// Print `lines` on stderr and exit 1 — boot's hard-error path (spec §6.9).
/// Exits directly so no extra `Error: …` line follows the exact copy.
fn exit_with(lines: &[String]) -> ! {
    for line in lines {
        eprintln!("{line}");
    }
    std::process::exit(1);
}

/// Spec §4.1 CLI rendering: `"{file}: {path}: {message}"`, one per error,
/// `{file}` the absolute path of the file the error is in.
fn render_config_errors(errors: &ConfigErrors, global: &Path, repo: &Path) -> Vec<String> {
    errors
        .0
        .iter()
        .map(|e| {
            let file = match e.file {
                ConfigFileKind::Global => global,
                ConfigFileKind::Repo => repo,
            };
            let path = e.dotted_path();
            if path.is_empty() {
                format!("{}: {}", file.display(), e.message)
            } else {
                format!("{}: {path}: {}", file.display(), e.message)
            }
        })
        .collect()
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

/// Build a `ServicePlan` from CLI `service install` arguments.
///
/// The loopback/token invariant is checked against what the service will
/// actually run with: the explicit `--bind`, else `general.web.bind`, else
/// `127.0.0.1:7823`; the explicit `--token`, else `general.web.token`. The
/// config is read only when it parses (a missing or broken file just means
/// the defaults). Only explicit flags go on the argv (§8.4); the artifact
/// never embeds the secret.
fn build_plan_for_install(
    bind: Option<std::net::SocketAddr>,
    token: Option<&str>,
    tls: bool,
    tls_names: Vec<String>,
) -> Result<(ServicePlan, Option<String>), anyhow::Error> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let web = Config::load(&cwd, RepoMode::Ignore, &ParseCtx { known_skills: &[] })
        .ok()
        .map(|config| config.general.web);
    let effective_bind = bind
        .or_else(|| web.as_ref().map(|w| w.bind))
        .unwrap_or_else(|| {
            peakbot::ui::DEFAULT_WEB_ADDR
                .parse()
                .expect("DEFAULT_WEB_ADDR is a valid SocketAddr literal")
        });
    let effective_token = resolve_token(token, None, web.as_ref().and_then(|w| w.token.as_deref()));
    let (exe, fallback_note) = resolve_service_exe();
    let plan = ServicePlan::new(exe, effective_bind, effective_token)?
        .with_bind_in_argv(bind.is_some())
        .with_tls(tls)
        .with_tls_names(tls_names);
    Ok((plan, fallback_note))
}

/// `service install --token T` (spec §8.4): migrate a legacy config.yaml
/// first (exactly like boot), then set `general.web.token: T`, validate
/// through `Config::parse` and save. Returns the lines to print.
fn write_token_to_config(config_dir: &Path, token: &str) -> anyhow::Result<Vec<String>> {
    let mut lines = auto_migrate(config_dir, None)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .notices();

    let path = config_dir.join("config.yaml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => anyhow::bail!(
            "config.yaml not found at {} — start PeakBot once to set it up, then rerun \
             'peakbot service install --token …'.",
            path.display()
        ),
        Err(e) => anyhow::bail!("cannot read {}: {e}", path.display()),
    };
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    set_web_token(&mut doc, token)
        .ok_or_else(|| anyhow::anyhow!("{}: not a YAML mapping", path.display()))?;
    let new_text = serde_yaml::to_string(&doc)?;
    if let Err(errors) = Config::parse(
        Source {
            path: &path,
            text: &new_text,
        },
        None,
        &ParseCtx { known_skills: &[] },
    ) {
        let repo = PathBuf::new();
        anyhow::bail!(
            "config.yaml would not be valid with the token set:\n{}",
            render_config_errors(&errors, &path, &repo).join("\n")
        );
    }
    peakbot::config::save_config_at(config_dir, &new_text)?;
    lines.push(format!(
        "Wrote general.web.token to {} (comments in config.yaml are not preserved; \
         previous file: config.yaml.bak).",
        path.display()
    ));
    Ok(lines)
}

/// Set `general.web.token`, creating the `general` / `web` mappings as
/// needed. `None` when the document (or one of those keys) is not a mapping.
fn set_web_token(doc: &mut serde_yaml::Value, token: &str) -> Option<()> {
    use serde_yaml::{Mapping, Value};
    let mut node = doc;
    for key in ["general", "web"] {
        let map = node.as_mapping_mut()?;
        node = map
            .entry(Value::from(key))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if node.is_null() {
            *node = Value::Mapping(Mapping::new());
        }
    }
    node.as_mapping_mut()?
        .insert(Value::from("token"), Value::from(token));
    Some(())
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
            ServiceAction::Install {
                bind,
                token,
                tls,
                tls_name,
            } => {
                // An explicit --token goes into config.yaml first, so the
                // plan below validates against the config the service will
                // read. A migration or write failure stops the install.
                if let Some(t) = token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                    let config_dir = peakbot::config::get_config_dir()
                        .ok_or_else(|| anyhow::anyhow!("cannot determine the config directory"))?;
                    for line in write_token_to_config(&config_dir, t)? {
                        println!("{line}");
                    }
                }
                let (plan, fallback_note) =
                    build_plan_for_install(bind, token.as_deref(), tls, tls_name)
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
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
                let report = service_op(ServiceOp::Uninstall)
                    .map_err(|e| anyhow::anyhow!("service uninstall failed: {e}"))?;
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

/// Print a [`ServiceReport`]. The text is formatted by the install core
/// (B4: `format_service_report` is the desktop-parsed contract); `print!`
/// verbatim — the string already ends with a newline.
fn print_service_report(report: &peakbot::install::ServiceReport) {
    print!("{}", peakbot::install::format_service_report(report));
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

    if mode == Mode::Stdio {
        log_subscriber(std::io::stderr, mode).init();
    } else {
        log_subscriber(std::io::stdout, mode).init();
    }

    // Arm the `PEAKBOT_SNIFF` debug log before any client exists, so no LLM
    // call can slip past it. Off unless the env var names a path.
    peakbot::sniff::init_from_env();

    // ── Shared setup ──────────────────────────────────────────────

    let boot_cwd = std::env::current_dir().unwrap_or_default();
    let config_dir = peakbot::config::get_config_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine the config directory"))?;
    let global_path = config_dir.join("config.yaml");
    let repo_path = boot_cwd.join(".peakbot").join("config.yaml");

    // Step 0 (spec §6.9), every mode, before anything reads the config:
    // migrate a pre-0.24 global (and, unpinned, the boot cwd's repo file).
    // Only a windowed web session may boot into needs_setup on a failed
    // GLOBAL migration (spec §6.9); every other mode exits.
    let windowed_web = mode == Mode::Web && peakbot::ui::web::windowed_session();
    let (boot_notices, migrate_error) = match auto_migrate(
        &config_dir,
        cli.profile.is_none().then_some(boot_cwd.as_path()),
    ) {
        Ok(outcome) => (outcome.notices(), None),
        Err(e) if windowed_web && e.file == ConfigFileKind::Global => {
            eprintln!("{e}");
            (Vec::new(), Some(e.to_string()))
        }
        Err(e) => exit_with(&[e.to_string(), CANNOT_START_WITH_FILE.to_string()]),
    };
    for notice in &boot_notices {
        eprintln!("{notice}");
    }

    // A failed global migration in a windowed web session: there is no
    // Config to load — boot the needs_setup server (spec §6.9) and let the
    // SPA show the reason (there is no migrate endpoint).
    if let Some(migrate_error) = migrate_error {
        return run_needs_setup_web(&cli, &config_dir, &boot_cwd, Some(migrate_error)).await;
    }

    // Load skills relative to the boot cwd (an allowed mint-site read of the
    // process cwd): `Config::parse` checks skill names against them. Each
    // session rescans for its own cwd and surfaces the warnings itself.
    let (skills, skill_warnings) = load_default_skills(&boot_cwd);
    for w in &skill_warnings {
        tracing::warn!("{w}");
    }

    // A pin names a global profile, so repo files are ignored under it.
    let repo_mode = if cli.profile.is_some() {
        RepoMode::Ignore
    } else {
        RepoMode::Include
    };
    let known_skills = skills.names();
    let parse_ctx = ParseCtx {
        known_skills: &known_skills,
    };
    let config = match Config::load(&boot_cwd, repo_mode, &parse_ctx) {
        Ok(config) => config,
        Err(LoadError::Missing(path)) => {
            // §6.9: a windowed web session with no global file boots the
            // settings server (needs_setup) — there is no Config yet.
            if windowed_web {
                return run_needs_setup_web(&cli, &config_dir, &boot_cwd, None).await;
            }
            print_config_not_found_message(&path);
            anyhow::bail!("No config file found. See instructions above.");
        }
        // Only reachable if the file changed after step 0.
        Err(LoadError::Legacy(path)) => exit_with(&[format!(
            "{}: {}",
            path.display(),
            peakbot::config::resolve::L1_MESSAGE
        )]),
        Err(LoadError::Invalid(errors)) => {
            exit_with(&render_config_errors(&errors, &global_path, &repo_path))
        }
        Err(LoadError::Io(path, e)) => exit_with(&[format!("cannot read {}: {e}", path.display())]),
    };
    for warning in &config.warnings {
        eprintln!("warning: {warning}");
    }
    let start = match config.catalog.start_bound(cli.profile.as_deref()) {
        Ok(bound) => bound,
        Err(message) => exit_with(&[message]),
    };

    // Boot-only: every HTTP client built from here on inherits these.
    peakbot::http::init_timeouts(config.general.http.clone());

    // Detect the shell first — the system prompt needs it so the model is
    // told which syntax + shell tool to use (#82). On Windows with no shell
    // found, warn and continue; other tools still work.
    let shell_kind = ShellKind::detect();
    if let Some(ref sk) = shell_kind {
        tracing::info!("Detected shell: {} ({})", sk.name(), sk.executable());
    } else {
        print_no_shell_warning();
    }

    // One set of MCP subprocesses for the process. Each session re-derives
    // its tools list from the shared handles (McpTool: Clone), filtered by
    // its profile — see `ToolEnv`.
    let mcp_handles = Arc::new(load_mcp_servers(&config).await?);

    // Create shared conversation storage if enabled. One storage instance
    // is shared by every session — it writes distinct files per
    // conversation id (see session::SessionDeps).
    let conversation = &config.general.conversation;
    let storage: Option<Arc<dyn peakbot::ConversationStorage>> = if conversation.auto_save {
        let storage_dir = conversation.storage_dir_or_default();
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

    let registry = config.registry.clone();
    let active_alias = start.agent().main.model.alias.clone();
    let web = config.general.web.clone();
    let uploads = peakbot::attachments::UploadStore::new(
        peakbot::attachments::UploadStore::default_root(),
        config.general.uploads,
    );

    // Build the shared session deps once. Each session (the single
    // TUI/stdio session, or one per web-socket connection) is built from
    // these via `create_session`. `Arc` so the web UI can hand a clone to
    // every connection handler. See `src/session.rs`.
    let session_deps = Arc::new(peakbot::SessionDeps {
        config: Arc::new(config),
        skills,
        mcp_handles,
        boot_notices: std::sync::Mutex::new(boot_notices),
        shell_kind,
        storage,
        profile_pin: cli.profile.clone(),
        uploads,
    });

    use peakbot::ui::{ReplUi, StdioUi, WebUi, build_models_snapshot};

    // The web UI builds one session per WebSocket connection, so it takes
    // the deps and never boots a single shared session. The TUI and stdio
    // views are single-session: build one, drive it, drop it (teardown).
    if mode == Mode::Web {
        // Bind, token and TLS (spec §8.4): CLI first, then `general.web`.
        // Parse-at-the-boundary: validate here, then trust the
        // Option<token> inside WebUi.
        let addr = cli.bind.unwrap_or(web.bind);
        let env_token = std::env::var("PEAKBOT_WEB_TOKEN").ok();
        let token = resolve_token(
            cli.token.as_deref(),
            env_token.as_deref(),
            web.token.as_deref(),
        );

        // An agent that can run shell commands must never be reachable on a
        // non-loopback address without a secret. Make the footgun impossible.
        if !addr.ip().is_loopback() && token.is_none() {
            anyhow::bail!(
                "refusing to bind the web UI to non-loopback {addr} without a token — \
                 set --token, PEAKBOT_WEB_TOKEN, or general.web.token"
            );
        }

        let tls = cli.tls || web.tls;
        // The settings surface's boot facts (spec §8.2 `boot` block): the
        // token source mirrors `resolve_token`'s chain. A failed global
        // migration already returned the needs_setup server above, so
        // `migrate_error` is None here.
        let settings = peakbot::ui::web::settings::SettingsBoot {
            config_dir: config_dir.clone(),
            boot_cwd: boot_cwd.clone(),
            migrate_error: None,
            token_source: peakbot::ui::web::settings::TokenSource::resolve(
                cli.token.as_deref(),
                env_token.as_deref(),
                web.token.as_deref(),
            ),
        };
        let mut ui = WebUi::new(
            addr,
            Some(session_deps.clone()),
            build_models_snapshot(&registry),
            active_alias,
            token,
            tls,
            cli.tls_name.clone(),
            settings,
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
            build_models_snapshot(&registry),
            boot_alias.clone(),
        );
        ui.init().await?;
        ui.run().await?;
        ui.shutdown().await?;
    } else {
        // The registry lets the View intercept/validate `/model <alias>`
        // before any UiAction is dispatched.
        let mut ui =
            ReplUi::new_with_registry(state_manager.clone(), action_sender, registry.clone());
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

/// §6.9 needs_setup server: the web UI without session deps — the SPA,
/// `/api/settings`, `/api/config*` and `/api/system/*`, with `/ws` the 503
/// setup guard. Reached from a windowed web session whose global file is
/// missing, or whose boot auto-migration failed (`migrate_error` carries
/// the reason verbatim). Bind/token/TLS resolve without a config: CLI
/// flags and env only, the same non-loopback refusal as normal boot.
async fn run_needs_setup_web(
    cli: &Cli,
    config_dir: &Path,
    boot_cwd: &Path,
    migrate_error: Option<String>,
) -> Result<()> {
    let addr = cli.bind.unwrap_or_else(|| {
        peakbot::ui::DEFAULT_WEB_ADDR
            .parse()
            .expect("DEFAULT_WEB_ADDR is a valid SocketAddr literal")
    });
    let env_token = std::env::var("PEAKBOT_WEB_TOKEN").ok();
    let token = resolve_token(cli.token.as_deref(), env_token.as_deref(), None);
    if !addr.ip().is_loopback() && token.is_none() {
        anyhow::bail!(
            "refusing to bind the web UI to non-loopback {addr} without a token — \
             set --token, PEAKBOT_WEB_TOKEN, or general.web.token"
        );
    }
    let tls = cli.tls;
    let settings = peakbot::ui::web::settings::SettingsBoot {
        config_dir: config_dir.to_path_buf(),
        boot_cwd: boot_cwd.to_path_buf(),
        migrate_error,
        token_source: peakbot::ui::web::settings::TokenSource::resolve(
            cli.token.as_deref(),
            env_token.as_deref(),
            None,
        ),
    };
    let mut ui = peakbot::ui::WebUi::new(
        addr,
        None,
        Vec::new(),
        String::new(),
        token,
        tls,
        cli.tls_name.clone(),
        settings,
    );
    ui.init().await?;
    ui.run().await?;
    ui.shutdown().await?;
    Ok(())
}

/// The `tracing` filter directives: `RUST_LOG` verbatim when set, else the mode's
/// default. Web/stdio record `peakbot` warnings (e.g. sub-agent retries); the TUI
/// stays errors-only because its logs share the stdout that ratatui draws on.
fn log_filter(env: Option<&str>, mode: Mode) -> String {
    match (env, mode) {
        (Some(directives), _) => directives.to_string(),
        (None, Mode::Tui) => "error".to_string(),
        (None, Mode::Web | Mode::Stdio) => "error,peakbot=warn".to_string(),
    }
}

/// Runtime tracing subscriber. `log_internal_errors(false)`: a failed log write is
/// dropped, never reported via `eprintln!` — which panics when stderr is gone.
fn log_subscriber<W>(writer: W, mode: Mode) -> impl tracing::Subscriber + Send + Sync + 'static
where
    W: for<'w> tracing_subscriber::fmt::MakeWriter<'w> + Send + Sync + 'static,
{
    let directives = log_filter(std::env::var("RUST_LOG").ok().as_deref(), mode);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::builder().parse_lossy(directives))
        .with_writer(writer)
        .log_internal_errors(false)
        .finish()
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
                action: ServiceAction::Install { bind, token, .. },
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
                    token: None,
                    ..
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

    // ── #375 — `service install --tls` / `--tls-name`. ────────────

    #[test]
    fn cli_service_install_rejects_tls_name_without_tls() {
        // #375 criterion 3: mirrors the top-level `requires = "tls"` —
        // `--tls-name` alone is a clap error (non-zero exit upstream).
        // Green today for the *wrong* reason (the flag does not exist
        // yet, so clap rejects it as unknown) and must STAY green once
        // the flag lands with `requires = "tls"`.
        assert!(parse(&["service", "install", "--tls-name", "x"]).is_err());
    }

    #[test]
    fn cli_service_install_parses_tls_and_repeatable_tls_names() {
        let cli = parse(&[
            "service",
            "install",
            "--bind",
            "127.0.0.1:7823",
            "--tls",
            "--tls-name",
            "a.lan",
            "--tls-name",
            "10.0.0.9",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Service {
                action: ServiceAction::Install { tls, tls_name, .. },
            }) => {
                assert!(tls, "--tls must be set");
                assert_eq!(
                    tls_name,
                    vec!["a.lan".to_string(), "10.0.0.9".to_string()],
                    "names must round-trip in the given order"
                );
            }
            other => panic!("expected Service::Install, got {other:?}"),
        }
    }

    #[test]
    fn cli_service_install_defaults_tls_off_without_flags() {
        // #375 criterion 7 (parse boundary): without the new flags the
        // result is unchanged — tls off, no names.
        let cli = parse(&["service", "install"]).unwrap();
        match cli.command {
            Some(Command::Service {
                action: ServiceAction::Install { tls, tls_name, .. },
            }) => {
                assert!(!tls, "tls must default to off");
                assert!(tls_name.is_empty(), "no names by default");
            }
            other => panic!("expected Service::Install, got {other:?}"),
        }
    }

    #[test]
    fn set_web_token_creates_missing_general_and_web_maps() {
        let mut doc: serde_yaml::Value = serde_yaml::from_str("providers: []\n").unwrap();
        set_web_token(&mut doc, "t0k").unwrap();
        assert_eq!(doc["general"]["web"]["token"].as_str(), Some("t0k"));
    }

    #[test]
    fn set_web_token_keeps_sibling_keys_and_replaces_the_old_token() {
        let mut doc: serde_yaml::Value =
            serde_yaml::from_str("general:\n  web:\n    tls: true\n    token: old\n").unwrap();
        set_web_token(&mut doc, "new").unwrap();
        assert_eq!(doc["general"]["web"]["token"].as_str(), Some("new"));
        assert_eq!(doc["general"]["web"]["tls"].as_bool(), Some(true));
    }

    #[test]
    fn set_web_token_refuses_a_non_mapping_document() {
        let mut doc = serde_yaml::Value::String("nope".into());
        assert!(set_web_token(&mut doc, "t").is_none());
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
        // CI containers (e.g. `rust:1.95` as root) don't have
        // `/root/.local/bin` yet, and `std::fs::write` does not create
        // parent directories. Make the dir so the sentinel write
        // succeeds; the guard's `Drop` only removes the file, so the
        // directory persists (matching CI-container reality).
        std::fs::create_dir_all(target.parent().expect("install target has a parent"))
            .expect("create install-target parent dir");
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

    // ── Dead-writer regression (agent-loop panic wedge, fix C1) ─────────
    //
    // A web-mode PeakBot wedged forever when its stdout/stderr were a dead
    // pty (EIO): a `tracing::error!` failed to write, tracing-subscriber's
    // default `log_internal_errors(true)` reported the failure via
    // `eprintln!`, and that `eprintln!` panicked on the dead stderr —
    // killing the agent-loop task and leaving `is_running` true forever.
    //
    // The parent test re-runs this test binary with fd 2 on `/dev/full`.
    // libtest captures eprintln! in-process by default, so the child runs
    // with `--nocapture` to make the write hit the real (dead) fd.

    /// A writer that fails every write with EIO — a dead pty in miniature.
    struct DeadWriter;
    impl std::io::Write for DeadWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from_raw_os_error(5)) // EIO
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    #[ignore = "spawned by log_subscriber_never_panics_when_stdout_and_stderr_are_dead"]
    fn log_subscriber_dead_writer_child() {
        let sub = log_subscriber(|| DeadWriter, Mode::Web);
        tracing::subscriber::with_default(sub, || tracing::error!("write fails; must not panic"));
    }

    #[test]
    fn log_filter_defaults_record_peakbot_warnings_except_in_the_tui() {
        assert_eq!(log_filter(None, Mode::Web), "error,peakbot=warn");
        assert_eq!(log_filter(None, Mode::Stdio), "error,peakbot=warn");
        // The TUI draws on stdout, the same stream the subscriber writes to.
        assert_eq!(log_filter(None, Mode::Tui), "error");
    }

    #[test]
    fn log_filter_passes_rust_log_through_verbatim() {
        for mode in [Mode::Web, Mode::Tui, Mode::Stdio] {
            assert_eq!(log_filter(Some("peakbot=debug"), mode), "peakbot=debug");
            assert_eq!(log_filter(Some(""), mode), "");
        }
    }

    /// Captures formatted log lines so the default filter is checked through
    /// the real subscriber, not just as a string.
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn default_filter_keeps_peakbot_warnings_and_drops_other_crates() {
        // RUST_LOG is process-global; the filter fn is what the default path uses.
        let capture = Capture::default();
        let sink = capture.clone();
        let sub = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::builder().parse_lossy(log_filter(None, Mode::Web)))
            .with_writer(move || sink.clone())
            .finish();
        tracing::subscriber::with_default(sub, || {
            tracing::warn!(target: "peakbot", "retry-warning");
            tracing::warn!(target: "hyper", "other-crate-warning");
            tracing::info!(target: "peakbot", "peakbot-info");
            tracing::error!(target: "hyper", "other-crate-error");
        });
        let out = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(out.contains("retry-warning"), "got: {out}");
        assert!(out.contains("other-crate-error"), "got: {out}");
        assert!(!out.contains("other-crate-warning"), "got: {out}");
        assert!(!out.contains("peakbot-info"), "got: {out}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn log_subscriber_never_panics_when_stdout_and_stderr_are_dead() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "tests::log_subscriber_dead_writer_child",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("RUST_LOG", "error")
            .stderr(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/full")
                    .unwrap(),
            )
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("1 passed") || stdout.contains("1 failed"),
            "child must actually run; got: {stdout}"
        );
        assert!(
            out.status.success(),
            "logging panicked on a dead writer/stderr: {stdout}"
        );
    }
}
