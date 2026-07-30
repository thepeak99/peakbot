import type { Welcome } from "../types";

function Pill({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline gap-1.5">
      <span className="text-zinc-500">{label}</span>
      <span className="font-mono text-zinc-300">{value}</span>
    </div>
  );
}

// The one-time startup banner. Mirrors WelcomeState (src/ui/app_state.rs):
// provider/model, tool + skill counts, and the feature toggles the TUI prints.
//
// The version badge in the header reads `WelcomeState::peakbot_version`, the
// same field the Session tab shows. Both flow from `crate::PEAKBOT_VERSION`
// at startup, so this string always matches the binary that served the page.
export function WelcomeBanner({ welcome }: { welcome: Welcome }) {
  return (
    <div className="rounded-lg border border-zinc-800 bg-gradient-to-b from-zinc-900/80 to-zinc-950/40 p-4">
      <div className="mb-3 flex items-center gap-2">
        <span className="text-lg">✦</span>
        <h2 className="text-base font-semibold text-zinc-100">Welcome to Shifu</h2>
        <span className="rounded bg-emerald-950/60 px-1.5 py-0.5 text-[10px] font-medium text-emerald-400">
          ready
        </span>
        {welcome.peakbotVersion && (
          <span
            className="rounded bg-zinc-800 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400"
            title={`Shifu v${welcome.peakbotVersion}`}
          >
            v{welcome.peakbotVersion}
          </span>
        )}
      </div>
      <div className="grid grid-cols-2 gap-x-6 gap-y-1.5 text-xs sm:grid-cols-3">
        <Pill label="provider" value={welcome.provider} />
        <Pill label="model" value={welcome.model} />
        <Pill label="max tokens" value={welcome.maxTokens.toLocaleString()} />
        <Pill label="built-in tools" value={String(welcome.builtinTools)} />
        <Pill label="MCP tools" value={String(welcome.mcpTools)} />
        <Pill label="skills" value={String(welcome.skills)} />
        <Pill label="web search" value={welcome.searxngEnabled ? "on" : "off"} />
        <Pill label="cost tracking" value={welcome.costTracking ? "on" : "off"} />
        <Pill
          label="compaction"
          value={
            welcome.compactionEnabled
              ? `${Math.round(welcome.compactionThreshold * 100)}%`
              : "off"
          }
        />
      </div>
      <div className="mt-3 border-t border-zinc-800 pt-2 text-xs text-zinc-500">
        <span className="font-mono text-zinc-400">{welcome.cwd}</span>
      </div>
    </div>
  );
}
