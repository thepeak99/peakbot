import type {
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
} from "../state";
import type { CatalogProfile, Selection } from "../types";
import { ModelSwitcher } from "./ModelSwitcher";
import { CwdPicker } from "./CwdPicker";
import { ConversationsPicker } from "./ConversationsPicker";
import { SelectionChip } from "./SelectionChip";
import { AgentsChip } from "./AgentsChip";
import { boundAgentOf } from "../adapt";

// Bottom status bar — mobile-only (hidden on lg+). Houses the selection chip,
// conversations picker, model switcher, agents chip, and cwd picker so all
// session-affecting controls are reachable without opening a drawer.
// Menus open upward (`dropUp`) so they don't clip off the bottom of the
// viewport.
export function BottomBar({
  conversations,
  conversationsError,
  models,
  activeAlias,
  hasTranscript,
  cwd,
  dirListing,
  recentDirs,
  send,
  onSwitchModel,
  onLoadConversation,
  catalog,
  selection,
  profilePinned = false,
  onToast,
}: {
  conversations: ConversationSummary[];
  conversationsError: string | null;
  models: ModelInfo[];
  activeAlias: string;
  hasTranscript: boolean;
  cwd: string | null;
  dirListing: DirListing | null;
  recentDirs: string[];
  send: (msg: InboundMessage) => void;
  onSwitchModel: (alias: string) => void;
  onLoadConversation: (id: string) => void;
  // T32 chat chips (optional — absent ⇒ no chips, as on older servers).
  catalog?: CatalogProfile[];
  selection?: Selection | null;
  /** `AppState.profile_pinned` — a --profile pin freezes the profile. */
  profilePinned?: boolean;
  /** Toast sink for `Next conversation: …` (agent pick). */
  onToast?(message: string): void;
}) {
  const boundAgent = boundAgentOf(catalog, selection);
  return (
    <footer className="flex min-h-14 items-center gap-2 border-t border-zinc-800 bg-zinc-950/80 px-3 py-2 pb-[max(0.5rem,env(safe-area-inset-bottom))] backdrop-blur lg:hidden">
      <SelectionChip
        catalog={catalog ?? []}
        selection={selection ?? null}
        locked={hasTranscript}
        pinned={profilePinned}
        send={send}
        onToast={onToast}
        dropUp
      />
      <ConversationsPicker
        conversations={conversations}
        error={conversationsError}
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
      <AgentsChip
        profile={selection?.profile ?? null}
        agent={boundAgent}
        activeModel={activeAlias}
        dropUp
      />

      {cwd && (
        <CwdPicker
          cwd={cwd}
          hasTranscript={hasTranscript}
          dirListing={dirListing}
          recentDirs={recentDirs}
          send={send}
          dropUp
        />
      )}
    </footer>
  );
}
