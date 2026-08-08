// The transcript's two floating scroll affordances, bottom-right of the
// transcript pane. Positioned absolutely by the caller's `relative` wrapper —
// *outside* the scrolling element, or they'd scroll away with the content.
//
// The "go to bottom" control and the "N new messages" pill are one button: it
// only appears when the user is unpinned, and it grows a count when messages
// arrived while they were reading history.

interface Props {
  /** Following the newest message — the bottom button is pointless then. */
  pinned: boolean;
  /** Messages that arrived since the user scrolled away (0 while pinned). */
  unread: number;
  showTop: boolean;
  onBottom: () => void;
  onTop: () => void;
  /** Side drawer is open — shift the buttons left so the drawer doesn't cover them. */
  drawerOpen: boolean;
}

const BUTTON =
  "cursor-pointer rounded-full border border-zinc-800 bg-zinc-900 px-3 py-1.5 text-xs text-zinc-300 shadow-xl transition-colors hover:bg-zinc-800 hover:text-zinc-100";

export function TranscriptNav({
  pinned,
  unread,
  showTop,
  onBottom,
  onTop,
  drawerOpen,
}: Props) {
  return (
    <div
      className={`pointer-events-none absolute bottom-4 right-14 z-10 flex flex-col items-end gap-2 transition-[right] duration-300 ease-out ${
        drawerOpen ? "lg:right-[344px]" : ""
      }`}
    >
      {showTop && (
        <button
          onClick={onTop}
          aria-label="Scroll to the top of the loaded transcript"
          className={`pointer-events-auto ${BUTTON}`}
        >
          ↑ top
        </button>
      )}
      {!pinned && (
        <button
          onClick={onBottom}
          aria-label={
            unread > 0
              ? `Scroll to ${unread} new message${unread === 1 ? "" : "s"}`
              : "Scroll to the newest message"
          }
          className={`pointer-events-auto ${BUTTON}`}
        >
          ↓{" "}
          {unread > 0
            ? `${unread} new message${unread === 1 ? "" : "s"}`
            : "bottom"}
        </button>
      )}
    </div>
  );
}
