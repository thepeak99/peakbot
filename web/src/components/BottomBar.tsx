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

// Bottom status bar — mobile-only (hidden on lg+), except in the Chats home
// shell (`mobileChats`): there the chat header owns the selection chip and the
// Chats list replaces the conversations picker, so the bar holds only the
// CwdPicker and renders nothing without a `cwd`. The browser phone shell keeps
// chip + conversations; its cwd lives in the Directory tab. Menus open upward.
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
  /** Chats home shell: drop the chip, the conversations picker and the
   * `lg:hidden` gate; render null without a `cwd`. */
  mobileChats?: boolean;
}) {
  if (mobileChats && !cwd) return null;

  return (
    // flex-wrap (T34b): at ≤800 px the chips must not overflow the bar
    // horizontally — they wrap to a second row instead. `lg:pr-12` clears
    // the fixed tab rail on the right (desktop only; phones have no rail).
    <footer
      data-testid="bottom-bar"
      className={`flex min-h-14 flex-wrap items-center gap-2 border-t border-zinc-800 bg-zinc-950/80 pl-3 lg:pr-12 py-2 pb-[max(0.5rem,env(safe-area-inset-bottom))] backdrop-blur${
        mobileChats ? "" : " lg:hidden"
      }`}
    >
      {/* The Chats home shell hosts the chip in the chat header and renders
          the Chats list instead of the picker. */}
      {!mobileChats && (
        <>
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
        </>
      )}

      {mobileChats && cwd && (
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
