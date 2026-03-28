# PeakBot Software Development Team Pipeline

A multi-agent pipeline demonstrating a software development team with 5 specialized roles.

## Roles

| Agent | Role | Responsibility |
|-------|------|----------------|
| **orchestrator** | Team Lead | Entry point, coordinates the team, reviews requirements |
| **pm** | Product Manager | Creates product specifications, clarifies requirements |
| **architect** | Software Architect | Designs system architecture, makes technical decisions |
| **developer** | Software Developer | Implements features, writes clean code |
| **tester** | QA Engineer | Writes tests, validates implementation |

## Workflow

```
User → orchestrator → pm (product spec)
                  → architect (system design)
                  → developer (implementation)
                  → tester (tests)
                  → orchestrator (synthesize & deliver)
```

## Usage

1. Copy the config to your PeakBot config directory:
   ```bash
   cp examples/pipeline-team/config.yaml ~/.config/peakbot/config.yaml
   ```

2. Set your OpenRouter API key:
   ```bash
   export OPENROUTER_API_KEY=sk-or-v1-your-key-here
   ```

3. Run PeakBot:
   ```bash
   cargo run --release
   ```

4. Describe what you want to build:
   ```
   I want a CLI tool that converts markdown to HTML
   ```

## How Delegation Works

The orchestrator uses the `delegate` tool to invoke specialists:

```
delegate(
  agent="pm",
  task="Create a product specification for a CLI markdown converter",
  mode="series"
)
```

The orchestrator decides the flow based on the request. For example:
- Simple feature → PM → Developer → Tester
- Complex system → PM → Architect → Developer → Tester

## Customization

Edit `config.yaml` to:
- Change models (e.g., use `google/gemini-2.0-flash-001` for cost savings)
- Adjust prompts to match your team's conventions
- Add/remove agents
- Change default timeouts
