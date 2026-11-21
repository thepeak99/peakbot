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

// Bottom status bar — phone layout only (hidden on desktop via lg:hidden).
// App renders this only when not in the mobile shell (chats === null), so the
// chip and conversations picker are always present. The CwdPicker shows only
// when cwd is provided (browsableCwd from App), following the same gating as
// the TopBar's picker.
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
  onReattach,
  onSwitchCwd,
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
  onLoadConversation: (id: string, profile: string | null) => void;
  /** Chip pick across local/remote: re-attach on (profile, agent). */
  onReattach?(profile: string, agent?: string): void;
  /** Commits a cwd picked in the CwdPicker (App routes busy/idle). */
  onSwitchCwd?(path: string): void;
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
    // horizontally — they wrap to a second row instead. `desktop:pr-12`
    // clears the fixed tab rail on the right (desktop only; phones have no rail).
    <footer
      data-testid="bottom-bar"
      className="flex min-h-14 flex-wrap items-center gap-2 border-t border-zinc-800 bg-zinc-950/80 pl-3 desktop:pr-12 py-2 lg:hidden backdrop-blur"
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
        onReattach={onReattach}
        dropUp
      />
      <ConversationsPicker
        conversations={conversations}
        error={conversationsError}
        hasTranscript={hasTranscript}
        onOpen={() => send({ type: "request_conversations" })}
        onLoad={onLoadConversation}
        onKill={(id, profile) =>
          send({ type: "kill_session", convo: id, ...(profile ? { profile } : {}) })
        }
        dropUp
        catalog={catalog}
      />
      {cwd && (
        <CwdPicker
          cwd={cwd}
          hasTranscript={hasTranscript}
          dirListing={dirListing}
          recentDirs={recentDirs}
          send={send}
          onSwitchCwd={onSwitchCwd ?? (() => {})}
          dropUp
        />
      )}
    </footer>
  );
}
