import type { DirListing, InboundMessage, ModelInfo } from "../state";
import { ModelSwitcher } from "./ModelSwitcher";
import { CwdPicker } from "./CwdPicker";

// Mobile-only bottom bar. Holds the model switcher and working-dir chips that
// the TopBar hides below lg, keeping them visible without crowding the header.
// Both chips open upward (`dropUp`) so their menus don't clip off-screen.
export function BottomBar({
  models,
  activeAlias,
  hasTranscript,
  cwd,
  dirListing,
  send,
  onSwitchModel,
}: {
  models: ModelInfo[];
  activeAlias: string;
  hasTranscript: boolean;
  cwd: string | null;
  dirListing: DirListing | null;
  send: (msg: InboundMessage) => void;
  onSwitchModel: (alias: string) => void;
}) {
  return (
    <footer className="flex items-center gap-3 border-t border-zinc-800 bg-zinc-950/80 px-4 py-2 backdrop-blur lg:hidden">
      <ModelSwitcher
        models={models}
        activeAlias={activeAlias}
        hasTranscript={hasTranscript}
        onSwitch={onSwitchModel}
        dropUp
      />

      {cwd && (
        <CwdPicker
          cwd={cwd}
          hasTranscript={hasTranscript}
          dirListing={dirListing}
          send={send}
          dropUp
        />
      )}
    </footer>
  );
}
