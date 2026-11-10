import type {
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
} from "../state";
import { isRemote } from "../state";
import type { CatalogProfile, Selection } from "../types";
import { CwdPicker } from "./CwdPicker";
import { ConversationsPicker } from "./ConversationsPicker";
import { SelectionChip } from "./SelectionChip";
import { edition } from "../desktop";

// Bottom status bar — mobile-only (hidden on lg+), except in the Chats home
// shell (`mobileChats`), where the bar shows at every width and the Chats list
// replaces the conversations picker. Menus open upward (`dropUp`).
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
  mobileChats = false,
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
  /** Chats home shell: drop the conversations picker and the `lg:hidden`
   * gate — the Chats list replaces the picker and the shell renders no
   * TopBar, so the bar must survive at tablet width. */
  mobileChats?: boolean;
}) {
  return (
    // flex-wrap (T34b): at ≤800 px the chips must not overflow the bar
    // horizontally — they wrap to a second row instead. `pr-12` clears the
    // fixed tab rail on the right (see TabbedDrawer).
    <footer
      data-testid="bottom-bar"
      className={`flex min-h-14 flex-wrap items-center gap-2 border-t border-zinc-800 bg-zinc-950/80 pl-3 pr-12 py-2 pb-[max(0.5rem,env(safe-area-inset-bottom))] backdrop-blur${
        mobileChats ? "" : " lg:hidden"
      }`}
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
      {/* The Chats home shell renders the Chats list instead of the picker. */}
      {!mobileChats && (
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
      )}

      {/* The Local edition has no cwd of its own (the shell owns it); a
          remote profile's cwd lives on the remote, reached via the relay. */}
      {cwd &&
        (edition() !== "local" || isRemote(selection?.profile ?? null, catalog ?? [])) && (
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
