//! PeakBot entry point

use anyhow::Result;
use clap::Parser;
use peakbot::{
    Config, FileStorage, ShellKind, SubAgentRegistry, Ui, build_system_prompt,
    get_config_file_path, load_default_skills, load_mcp_servers, print_no_shell_warning,
};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "peakbot")]
#[command(about = "PeakBot — AI coding assistant")]
#[command(version)]
struct Cli {
    /// Run the NDJSON stdin/stdout frontend instead of the terminal UI
    /// (for IDE integrations). stdout becomes the protocol channel, so logs
    /// go to stderr.
    #[arg(long)]
    stdio: bool,

    /// Run the web UI in a browser instead of the terminal UI. Phase 0
    /// ships the static shell (SPA + asset pipeline); Phase 1 adds live
    /// chat over WebSocket. The address is fixed (loopback only) for now;
    /// `--port` / `--bind` flags are Phase 4.
    #[arg(long)]
    web: bool,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    // `--stdio` must be known before tracing init: under it stdout is the
    // NDJSON wire, so logs MUST go to stderr or they corrupt the protocol.
    let cli = Cli::parse();

    let subscriber = tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env());
    if cli.stdio {
        subscriber.with_writer(std::io::stderr).init();
    } else {
        subscriber.init();
    }

    // ── Shared setup ──────────────────────────────────────────────

    // Load configuration with metadata about what was found
    let loaded = Config::load()?;

    // Check if we have no config file and no API key configured
    if !loaded.config_file_found && !has_api_key(&loaded.config) {
        // Show friendly error message with config path
        if let Some(config_path) = loaded.config_file_path {
            print_config_not_found_message(&config_path);
        } else if let Some(config_path) = get_config_file_path() {
            print_config_not_found_message(&config_path);
        }
        anyhow::bail!("No config file found. See instructions above.");
    }

    let mut config = loaded.config;
    let skills = load_default_skills()?;
    let skills_count = skills.len(); // Keep count before moving skills

    // Detect the shell first — the system prompt needs it so the model is
    // told which syntax + shell tool to use (#82). On Windows with no shell
    // found, warn and continue; other tools still work.
    let shell_kind = ShellKind::detect();
    let system_prompt = build_system_prompt(&skills, shell_kind.as_ref());

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

    // Pipeline. Shared across sessions via Arc.
    let pipeline_registry = if config.pipeline_enabled() {
        let pipeline_config = config.pipeline().unwrap();
        Some(Arc::new(SubAgentRegistry::new(pipeline_config)))
    } else {
        None
    };

    // Create provider — context window resolution lives in the registry
    // (per-model `context_size:` OR `auto_detect_context_size` against
    // the wire id). The provider itself doesn't need to know the value;
    // `ContextManager` is the single consumer downstream.
    //
    // Boot path: `resolve_and_mirror_boot_provider` looks up the active
    // model in the registry, copies its provider config into
    // `config.provider`, and hands back the resolved value. The mirror
    // keeps the invariant that `config.provider` == active provider,
    // matching the `/model` switch path at `lib.rs:1082`. Without it,
    // `AgentRunner::new`'s compaction-model construction reads the
    // stale `OpenRouterConfig::default()` (api_key=None) and falls over
    // with a misleading "OpenRouter API key not configured" error.
    let boot_provider_config = config.resolve_and_mirror_boot_provider(&model_registry);

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
                tracing::info!("Vector store enabled at: {}", vc.db_path);
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
        system_prompt: system_prompt.clone(),
        skills,
        mcp_handles: mcp_handles_arc.clone(),
        searxng_config,
        pipeline_registry,
        vector_store,
        shell_kind,
        boot_provider_config,
        storage,
        mcp_tools_count,
        skills_count,
    });

    use peakbot::ui::{ReplUi, StdioUi, WebUi, build_models_snapshot};

    // The web UI builds one session per WebSocket connection, so it takes
    // the deps and never boots a single shared session. The TUI and stdio
    // Views are single-session: build one, drive it, drop it (teardown).
    if cli.web {
        let addr: std::net::SocketAddr = peakbot::ui::DEFAULT_WEB_ADDR
            .parse()
            .expect("DEFAULT_WEB_ADDR is a valid SocketAddr literal");
        let active_alias = model_registry
            .default_alias()
            .map(|a| a.to_string())
            .unwrap_or_else(|| "default".to_string());
        let mut ui = WebUi::new(
            addr,
            session_deps.clone(),
            build_models_snapshot(&model_registry),
            active_alias,
        );
        ui.init().await?;
        ui.run().await?;
        ui.shutdown().await?;
        return Ok(());
    }

    // Single session for the TUI / stdio Views.
    let session = peakbot::create_session(&session_deps)?;
    let state_manager = session.state_manager.clone();
    let action_sender = session.action_sender.clone();
    let boot_alias = session.model_alias.clone();

    // Two single-session Views share the Model/Controller seam: `--stdio`
    // is the NDJSON frontend, default is the REPL.
    if cli.stdio {
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
