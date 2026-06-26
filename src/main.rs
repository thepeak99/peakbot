//! PeakBot entry point

use anyhow::Result;
use clap::Parser;
use peakbot::{
    AgentRunner, Config, FileStorage, ShellKind, SubAgentRegistry, TodoTool, Ui, UiAction,
    build_system_prompt, create_provider, ensure_ca_bundle, get_config_file_path,
    load_default_skills, load_mcp_servers, print_no_shell_warning,
};
use std::sync::Arc;
use tokio::sync::mpsc;
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
    // Point rustls at a compiled-in CA bundle when the host has no system
    // trust store (static musl binary on bare Android/Termux). Must run
    // before any TLS client is constructed. No-op when roots already exist.
    ensure_ca_bundle();

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
    // Wrap the handles in an Arc so the AgentRunner's RebuildContext
    // can re-derive the tools list across `/model` switches without
    // restarting the underlying subprocesses. McpTool: Clone (rig 0.33)
    // makes the per-build tool list cheap.
    let mcp_handles_arc = Arc::new(mcp_handles);
    let mcp_tools = if mcp_handles_arc.is_empty() {
        None
    } else {
        let mut all_tools = Vec::new();
        for handle in mcp_handles_arc.iter() {
            use rig_core::tool::ToolDyn;
            let tools: Vec<Box<dyn ToolDyn>> = handle
                .tools()
                .iter()
                .cloned()
                .map(|t| Box::new(t) as Box<dyn ToolDyn>)
                .collect();
            all_tools.extend(tools);
        }
        Some(all_tools)
    };

    let searxng_config = config
        .searxng_enabled()
        .then_some(config.searxng.clone())
        .flatten();

    // Create conversation storage if enabled
    let state_manager = if config.conversation_enabled() {
        let storage_dir = config.conversation_storage_dir();
        match FileStorage::new(storage_dir.clone()) {
            Ok(storage) => {
                tracing::info!("Conversation storage enabled at: {:?}", storage_dir);
                peakbot::StateManager::new_arc_with_storage(Arc::new(storage))
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to create conversation storage at {:?}: {}. \
                     Continuing without persistence.",
                    storage_dir,
                    e
                );
                peakbot::StateManager::new_arc()
            }
        }
    } else {
        peakbot::StateManager::new_arc()
    };

    let todo_tool = TodoTool::new(state_manager.clone());

    // Pipeline
    let pipeline_registry = if config.pipeline_enabled() {
        let pipeline_config = config.pipeline().unwrap();

        Some(SubAgentRegistry::new(pipeline_config))
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

    // Apply the detected shell (detection ran earlier so the system prompt
    // could reference it). On Windows with no shell, warn the user.
    if let Some(ref sk) = shell_kind {
        state_manager.set_shell(sk.executable().to_string());
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

    let (agent, provider_info, event_receiver, session_hook) = create_provider(
        &boot_provider_config,
        mcp_tools,
        &system_prompt,
        searxng_config.as_ref(),
        config.agent_max_turns,
        Some(todo_tool.clone()),
        &config.bash,
        pipeline_registry.as_ref(),
        state_manager.clone(),
        shell_kind.as_ref(),
        vector_store.as_ref(),
    )?;

    tracing::info!(
        "Using provider: {} with model: {}",
        provider_info.name,
        provider_info.model
    );

    // StateManager is already created above and shared with TodoTool.
    // Stamp the wire identity `(provider_name, model)` and the alias
    // (the active entry from the registry) so `/model` / `/load` /
    // status bar all see consistent values from boot. The wire id is
    // what gets persisted on conversations; the alias is display-only.
    state_manager.set_model(provider_info.model.clone());
    let (boot_provider_name, boot_alias) = match model_registry.default_alias() {
        Some(a) => match model_registry.resolve(a) {
            Some(rm) => (rm.provider_name.clone(), rm.alias.clone()),
            None => (String::new(), a.to_string()),
        },
        None => (String::new(), "default".to_string()),
    };
    state_manager.set_provider_name(boot_provider_name);
    state_manager.set_model_alias(boot_alias.clone());

    // Channel: View → Controller
    let (action_sender, action_receiver) = mpsc::unbounded_channel::<UiAction>();

    // Build the rebuild context so /model can rebuild between turns
    // without restarting the process. MCP handles, system prompt,
    // builtins config, and the registry are all kept alive here.
    let rebuild_ctx = peakbot::RebuildContext {
        registry: model_registry.clone(),
        system_prompt: system_prompt.clone(),
        mcp_handles: mcp_handles_arc.clone(),
        searxng_config: config.searxng.clone(),
        max_turns: config.agent_max_turns,
        todo_tool: Some(todo_tool),
        bash_config: config.bash.clone(),
        pipeline_registry: pipeline_registry.clone().map(Arc::new),
        shell_kind,
        vector_store,
    };

    // Resolve the boot model's context_size (from config or auto-detected).
    // This value drives ContextManager compaction thresholds.
    let boot_context_size = model_registry
        .default_alias()
        .and_then(|a| model_registry.resolve(a))
        .map(|rm| rm.context_size)
        .unwrap_or_else(|| {
            tracing::warn!("No default model in registry, using auto-detect for context_size");
            peakbot::auto_detect_context_size(provider_info.model.as_str())
        });

    // Create AgentRunner (Controller)
    let mut runner = AgentRunner::new(
        agent,
        config.clone(),
        provider_info.clone(),
        skills,
        event_receiver,
        Some(state_manager.clone()),
        session_hook,
        boot_context_size,
    )?
    .with_rebuild_context(rebuild_ctx);

    // Set up welcome banner state
    state_manager.set_welcome(peakbot::ui::app_state::WelcomeState {
        provider_name: provider_info.name.clone(),
        model: provider_info.model.clone(),
        max_tokens: config.max_tokens() as usize,
        builtin_tools_count: 11, // file_create, file_str_replace, file_insert, file_read, bash, list_directory, fetch_url, fetch_page, think, todo, search
        mcp_tools_count,
        skills_count,
        searxng_enabled: config.searxng_enabled(),
        searxng_url: config.searxng.as_ref().map(|s| s.base_url.clone()),
        cost_tracking_enabled: config.supports_pricing() && config.cost_tracking,
        compaction_enabled: config.context.enabled,
        compaction_threshold: config.context.threshold,
        compaction_keep_recent: config.context.keep_recent,
        conversation_persistence_enabled: config.conversation_enabled(),
        cwd: std::env::current_dir().unwrap_or_default(),
    });

    use peakbot::ui::{ReplUi, StdioUi, build_models_snapshot};

    let runner_handle = tokio::spawn(async move {
        runner.run_loop(action_receiver).await;
    });

    // Both Views share the Model/Controller seam; `--stdio` only swaps the
    // View for the NDJSON frontend.
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

    runner_handle.abort();
    let _ = runner_handle.await;

    Ok(())
}
