import type {
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
} from "../state";
import type { CatalogProfile, Selection } from "../types";
import { CwdPicker } from "./CwdPicker";
import { ConversationsPicker } from "./ConversationsPicker";
import { SelectionChip } from "./SelectionChip";

// Bottom status bar — mobile-only (hidden on lg+). Houses the selection chip
// (profile, agent and team-model setup), conversations picker, and cwd picker
// so all session-affecting controls are reachable without opening a drawer.
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
  onLoadConversation,
  catalog,
  selection,
  selectionLocked = false,
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
  onLoadConversation: (id: string) => void;
  // T32 chat chip (optional — absent ⇒ no chip, as on older servers).
  catalog?: CatalogProfile[];
  selection?: Selection | null;
  /** The conversation has a user/agent turn (`conversationHasTurns`) — the
   * selection chip locks. Not `hasTranscript`: system notices don't lock. */
  selectionLocked?: boolean;
  /** `AppState.profile_pinned` — a --profile pin freezes the profile. */
  profilePinned?: boolean;
  /** Toast sink for `Next conversation: …`. */
  onToast?(message: string): void;
}) {
  return (
    // flex-wrap (T34b): at ≤800 px the chips must not overflow the bar
    // horizontally — they wrap to a second row instead. `pr-12` clears the
    // fixed tab rail on the right (see TabbedDrawer).
    <footer
      data-testid="bottom-bar"
      className="flex min-h-14 flex-wrap items-center gap-2 border-t border-zinc-800 bg-zinc-950/80 pl-3 pr-12 py-2 pb-[max(0.5rem,env(safe-area-inset-bottom))] backdrop-blur lg:hidden"
    >
      <SelectionChip
        catalog={catalog ?? []}
        selection={selection ?? null}
        models={models}
        activeAlias={activeAlias}
        locked={selectionLocked}
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
