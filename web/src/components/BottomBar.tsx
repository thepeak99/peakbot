import type {
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
} from "../state";
import { ModelSwitcher } from "./ModelSwitcher";
import { CwdPicker } from "./CwdPicker";
import { ConversationsPicker } from "./ConversationsPicker";

// Bottom status bar — mobile-only (hidden on lg+). Houses the conversations
// picker, the model switcher, and the cwd picker so all three
// session-affecting controls are reachable without opening a drawer.
// Menus open upward (`dropUp`) so they don't clip off the bottom of the
// viewport.
export function BottomBar({
  conversations,
  models,
  activeAlias,
  hasTranscript,
  cwd,
  dirListing,
  send,
  onSwitchModel,
  onLoadConversation,
  drawerOpen,
}: {
  conversations: ConversationSummary[];
  models: ModelInfo[];
  activeAlias: string;
  hasTranscript: boolean;
  cwd: string | null;
  dirListing: DirListing | null;
  send: (msg: InboundMessage) => void;
  onSwitchModel: (alias: string) => void;
  onLoadConversation: (id: string) => void;
  drawerOpen: boolean;
}) {
  return (
    <footer
      className={`flex min-h-14 items-center gap-2 border-t border-zinc-800 bg-zinc-950/80 px-3 py-2 pb-[max(0.5rem,env(safe-area-inset-bottom))] backdrop-blur transition-[padding] duration-300 ease-out lg:hidden ${
        drawerOpen ? "min-[420px]:pr-[288px]" : ""
      }`}
    >
      <ConversationsPicker
        conversations={conversations}
        hasTranscript={hasTranscript}
        onOpen={() => send({ type: "request_conversations" })}
        onLoad={onLoadConversation}
        onKill={(id) => send({ type: "kill_session", convo: id })}
        dropUp
      />
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
