// The message composer. Sends `{"type":"send_message"}` on Enter (Shift+Enter
// inserts a newline) and `{"type":"stop"}` while the agent is running. Slash
// commands ride send_message — the backend classifies them. Disabled until
// the WebSocket connects.
//
// While a sub-agent is running and pausable (`subAgent.pausable`), a
// Pause/Resume button sits next to Stop: it sends the explicit `{"type":
// "pause"}` / `{"type":"resume"}` frames (never a toggle) and relabels by the
// wire `pause` state — "⏸ Pause" while running, "▶ Resume" while pausing or
// paused. Stop stays as is and aborts everything, paused sub-agent included.
//
// **Mobile / touch:** on a coarse-pointer device (phone, tablet, or laptop in
// tablet mode) Enter inserts a newline — the on-screen return key is one
// mis-press away from sending a half-typed message, so the only path to send
// is the button. Desktop (fine pointer) keeps Enter-to-send. The check is on
// the *primary* input device, not the viewport width, so a Surface in laptop
// mode with a keyboard attached still gets Enter-to-send. While the agent
// runs with an empty box, the row shows icon-only Pause/Stop (labels would
// squeeze the textarea on phones); the moment there's content to send, the
// labelled "Queue" button takes over.
//
// A slash palette (fed by `GET /commands`, the single source of truth) opens
// while the input is a bare `/name` prefix. It's a flat filtered list — pick
// with ↑/↓ + Enter/Tab, dismiss with Esc. Accepting fills the input (adding a
// trailing space for commands that take args); the user still presses Enter
// to send, so nothing auto-fires.
//
// While a sub-agent view is selected (`watchingRole`), the box carries a
// one-line notice and a softened placeholder: the view is a transcript filter,
// input still goes to the orchestrator.
//
// Attachments: 📎, paste, or drop (DropOverlay, owned by App) add files of any
// type. Each uploads immediately (`useAttachments`, owned by ComposerSlot — one
// per tab) and shows as a chip above the input; submit sends only the uploaded
// ids, never bytes. Send is blocked until every chip is done and none is in
// error. Linux desktop app: an image-only paste with no file data falls back
// to `readClipboardImage()` (WebKit Bug 218519). On Android 📎 opens a menu
// (camera / gallery / file); elsewhere it opens the OS dialog.

import type { JSX } from "react";
import { useCallback, useEffect, useRef, useState, useLayoutEffect } from "react";
import { isAndroid } from "../platform";
import { isDesktop, readClipboardImage } from "../desktop";
import type { SlashCommand } from "../state";
import type { SubAgentRun } from "../types";
import type { Attachments } from "../useAttachments";
import { useMediaQuery } from "../useMediaQuery";
import { AttachmentChip, NO_VISION, visionWarning } from "./AttachmentChip";

// Composer height budget. The textarea rests at a single row and grows with
// its content up to a hard cap, past which it scrolls internally. Values are px.
const MIN_H = 40; // one text row — roughly line-height + vertical padding
const MAX_H = 192; // ~6 rows — beyond this the textarea scrolls internally
// Phone face (mobile fidelity batch 1, mock .ta): one 16px/22px row, 118px cap.
const PHONE_MIN_H = 44;
const PHONE_MAX_H = 118;

// Pressing a button moves focus off the textarea, collapsing the soft keyboard;
// the layout jump then cancels the tap. Cancel the focus change on both the
// pointer event and the compat mousedown (Android Chrome focuses on the latter).
const preventFocusSteal = (e: { preventDefault: () => void }) => e.preventDefault();
const keepTextareaFocus = { onPointerDown: preventFocusSteal, onMouseDown: preventFocusSteal };

// The attach menu's rows in order: label, the input it fires, and the
// phone's stroke icon (mock .amenu: camera / image / paperclip in a 24px
// box). Desktop (no `phone`) renders the rows text-only, as before.
const ATTACH_ITEMS: {
  testid: string;
  label: string;
  ref: "file" | "camera" | "gallery";
  icon: JSX.Element;
}[] = [
  {
    testid: "attach-menu-camera",
    label: "Take a photo",
    ref: "camera",
    icon: (
      <>
        <path d="M20 8h-2.35A4.5 4.5 0 0 0 13.2 5.6H10.8A4.5 4.5 0 0 0 6.35 8H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-9a2 2 0 0 0-2-2Z" />
        <circle cx="12" cy="14" r="3.5" />
      </>
    ),
  },
  {
    testid: "attach-menu-gallery",
    label: "Choose from gallery",
    ref: "gallery",
    icon: (
      <>
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <circle cx="9" cy="10" r="1.6" />
        <path d="M3 17l4.5-4.5L12 17l3-3 6 6" />
      </>
    ),
  },
  {
    testid: "attach-menu-file",
    label: "Pick a file",
    ref: "file",
    // The existing desktop Attach path — same paperclip, new menu.
    icon: (
      <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
    ),
  },
];

// Inert default so the Composer works standalone (no upload plumbing).
const NO_ATTACHMENTS: Attachments = {
  pending: [],
  add: () => {},
  remove: () => {},
  reset: () => {},
  ids: [],
  ready: true,
};

export interface ComposerProps {
  isRunning: boolean;
  connected: boolean;
  commands: SlashCommand[];
  onSend: (text: string, attachmentIds: string[]) => void;
  onStop: () => void;
  /** The currently-running sub-agent (view type), or null when the turn is
   * orchestrator-only / between sub-agent invocations. Drives the
   * Pause/Resume button. */
  subAgent?: SubAgentRun | null;
  /** Send `{"type":"pause"}` — explicit command, not a toggle. */
  onPause: () => void;
  /** Send `{"type":"resume"}`. */
  onResume: () => void;
  /** Label of the sub-agent view currently being watched, or null in the
   * global view. Purely a transcript filter — input always goes to the
   * orchestrator — so its only job here is to say so. */
  watchingRole?: string | null;
  /** Drop back to the global view (the notice's "Clear" link). */
  onClearWatch?: () => void;
  /** Server-side queue depth (`AppState.pending_input_count`). While running,
   * >0 means stopping would drop queued sends, so Stop warns about it. */
  pendingInput: number;
  /** Upload state, owned by ComposerSlot (one per tab). */
  attachments?: Attachments;
  /** `/r/<L>` proxy prefix of this tab's profile ("" = local); attachment
   * fallback URLs need it to route to the right server. */
  base?: string;
  /** `AppState.supports_vision` — drives the ⚠ on image chips. */
  supportsVision?: boolean;
  /** Gear-gate result (settingsGearEnabled). The `Ctrl+, settings` hint only
   * makes sense when the Settings entry points exist; default true so a
   * standalone Composer (no wire fields yet) keeps showing it. */
  settingsEnabled?: boolean;
  /** `AppState.stop_pending` — a graceful stop is already armed (first
   * press). The Stop control takes the escalation label and a subtle
   * pulse; a second press aborts immediately (the backend decides). */
  stopPending?: boolean;
  /** Parked tab: stay mounted (keeps text and palette state) but not shown. */
  hidden?: boolean;
  /** Phone face (mobile fidelity batch 1): the mock's composer look and
   * copy. Derived once in App as `!isDesktop`; false (the default) renders
   * today's desktop DOM exactly. */
  phone?: boolean;
  /** Single-composer shells (mobile + plain browser) only: park the
   * half-typed text per bound conversation — `null` = a fresh, not-yet-named
   * chat, a later `null` → id is that chat getting its server id (not a
   * switch). `undefined` = feature off (desktop keep-alive tabs). */
  draftKey?: string | null;
}

export function Composer({
  isRunning,
  connected,
  commands,
  onSend,
  onStop,
  subAgent = null,
  onPause,
  onResume,
  watchingRole,
  onClearWatch,
  pendingInput,
  attachments = NO_ATTACHMENTS,
  base = "",
  supportsVision = true,
  settingsEnabled = true,
  stopPending = false,
  hidden = false,
  phone = false,
  draftKey,
}: ComposerProps) {
  // Phone face gets its own height budget (mock .ta 16/22, 118px cap).
  const MIN_H_P = phone ? PHONE_MIN_H : MIN_H;
  const MAX_H_P = phone ? PHONE_MAX_H : MAX_H;
  const [text, setText] = useState("");
  const [selected, setSelected] = useState(0);
  const [dismissed, setDismissed] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  // True when the primary input is a touch device (phone, tablet, laptop in
  // tablet mode). Gates Enter-to-send so an accidental tap on the on-screen
  // return key never fires a half-typed message.
  const touchInput = useMediaQuery("(pointer: coarse)");
  // The attach menu exists only where the OS has no attach sheet of its own;
  // the UA is the one signal shared by the Android WebView and browsers.
  const android = isAndroid();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const cameraInputRef = useRef<HTMLInputElement>(null);
  const galleryInputRef = useRef<HTMLInputElement>(null);
  // The three attach inputs in menu order, so the menu can drive a pick by row.
  const attachInputRefs = { file: fileInputRef, camera: cameraInputRef, gallery: galleryInputRef };
  const attachButtonRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Grow the textarea with its content between `MIN_H` (one row) and `MAX_H`;
  // past the cap it scrolls. Runs on every value change and after submit clears
  // the field, so the box always snaps back to its single-row resting height.
  // Skipped while hidden: a display:none textarea measures 0 and would collapse.
  useLayoutEffect(() => {
    const el = textareaRef.current;
    if (!el || hidden) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(Math.max(el.scrollHeight, MIN_H_P), MAX_H_P)}px`;
  }, [text, hidden, MIN_H_P, MAX_H_P]);

  // Palette shows only while typing a bare command name (`/foo`, no space).
  const query = text.startsWith("/") && !text.includes(" ") ? text.slice(1) : null;
  const matches =
    query === null
      ? []
      : commands.filter((c) => c.name.startsWith(query.toLowerCase()));
  // The palette is a client-side autocomplete over the statically-fetched
  // command list — it must NOT depend on the live socket, or it blinks out
  // during the reconnect that `switchConvo` deliberately triggers (sticky
  // sessions). Sending is still gated on `connected` in `submit()`.
  const showPalette = !isRunning && !dismissed && matches.length > 0 && !menuOpen;
  const sel = Math.min(selected, matches.length - 1);

  const onPickFiles = (e: React.ChangeEvent<HTMLInputElement>) => {
    attachments.add(Array.from(e.target.files ?? []));
    e.target.value = ""; // reset so re-picking the same file fires onChange again
  };

  // The menu unmounts on pick, so the input click must stay synchronous inside
  // the tap for the OS to honour it as user activation.
  const pick = (ref: React.RefObject<HTMLInputElement | null>) => {
    setMenuOpen(false);
    ref.current?.click();
  };

  // Close the menu on a press outside it or on Escape. The 📎 button is
  // excluded from the pointerdown check so it keeps toggling.
  useEffect(() => {
    if (!menuOpen) return;
    const onPointerDown = (e: Event) => {
      const t = e.target;
      if (t instanceof Node && (menuRef.current?.contains(t) || attachButtonRef.current?.contains(t))) return;
      setMenuOpen(false);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [menuOpen]);

  const submit = () => {
    const trimmed = text.trim();
    if (!canSend) return;
    onSend(trimmed, attachments.ids);
    setText("");
    attachments.reset();
    setDismissed(false);
  };

  const accept = (cmd: SlashCommand) => {
    setText(`/${cmd.name}${cmd.takes_args ? " " : ""}`);
    setDismissed(true); // hide until the field is edited again
  };

  // The one text-change path (typing, and draft restore below): the palette
  // selection/dismissal must reset whenever the field's content changes.
  const onChange = useCallback((v: string) => {
    setText(v);
    setSelected(0);
    setDismissed(false);
  }, []);

  // Per-conversation drafts (single-composer shells: one Composer serves
  // every conversation, so a switch swaps text in place). Half-typed text is
  // parked in memory under its `draftKey`; an empty box deletes the entry,
  // which is also why a sent message (already cleared) is never restored.
  const drafts = useRef(new Map<string | null, string>());
  const prevDraftKey = useRef(draftKey);
  useLayoutEffect(() => {
    const prev = prevDraftKey.current;
    prevDraftKey.current = draftKey;
    // off, first key (feature just turned on: keep the text), mount, or no
    // switch — nothing to park or restore.
    if (draftKey === undefined || prev === undefined || prev === draftKey) return;
    // null → id is the server naming the fresh chat — not a switch — but only
    // while that chat holds text: an empty fresh chat moving to an existing
    // one is a plain switch (there is nothing to keep). `prevDraftKey` above
    // already remembers the minted id.
    if (prev === null && typeof draftKey === "string" && text !== "") return;
    const map = drafts.current;
    if (text) map.set(prev, text);
    else map.delete(prev);
    onChange(map.get(draftKey) ?? "");
  }, [draftKey, text, onChange]);

  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const cd = e.clipboardData;
    let list = Array.from(cd.files ?? []);
    if (list.length === 0) {
      // Some browsers expose a pasted image only via items; never union with files (Chrome lists both).
      list = Array.from(cd.items ?? [])
        .filter((it) => it.kind === "file")
        .map((it) => it.getAsFile())
        .filter((f): f is File => f !== null);
    }
    if (list.length > 0) {
      e.preventDefault();
      attachments.add(list);
      return;
    }
    // Linux desktop app: WebKitGTK ≤ 2.50 hands paste events no image data
    // (WebKit Bug 218519), so ask the app for the clipboard image. A paste
    // carrying text keeps the default; nothing is prevented — with no files
    // and no text the default inserts nothing.
    if (!isDesktop() || !/Linux/.test(navigator.userAgent)) return;
    if (cd.types.includes("text/plain")) return;
    readClipboardImage().then(
      (file) => {
        if (file) attachments.add([file]);
      },
      () => {}, // older app / command unavailable: today's no-op
    );
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (showPalette) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelected((s) => (s + 1) % matches.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelected((s) => (s - 1 + matches.length) % matches.length);
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !touchInput)) {
        // On desktop Enter accepts the highlighted match (and then the user
        // presses Enter again to send). On touch we deliberately let Enter
        // fall through to insert a newline — the user picks a suggestion
        // with a tap, the keyboard's return key is too easy to mis-press.
        e.preventDefault();
        accept(matches[sel]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setDismissed(true);
        return;
      }
    }
    // Enter sends only on desktop (fine pointer / keyboard). On touch the
    // return key inserts a newline — sending is button-only, by design.
    if (e.key === "Enter" && !e.shiftKey && !touchInput) {
      e.preventDefault();
      submit();
    }
  };

  // Single placeholder, no embedded instructions. Touch/desktop parity —
  // gestures and key bindings live in the hint row below (desktop only).
  // While watching a sub-agent lane it names the real recipient instead.
  // The phone face (mock) reads "Message" / "Queue a message…" instead of
  // "Type a message…".
  const placeholder = !connected
    ? "Connecting…"
    : watchingRole
      ? "Message the orchestrator…"
      : phone
        ? isRunning
          ? "Queue a message…"
          : "Message"
        : "Type a message…";

  // Content half of `canSend`, split out because it also picks which running
  // controls are mounted: empty box → icon-only Pause/Stop, content → the
  // labelled Queue button alone.
  const hasContent = !!text.trim() || attachments.ids.length > 0;
  // Chips still uploading (or errored) must not be silently left behind.
  const canSend = connected && hasContent && attachments.ready;
  const sendBlockedReason = !attachments.ready
    ? attachments.pending.some((c) => c.state === "error")
      ? "Remove the failed attachment to send"
      : "Waiting for uploads to finish"
    : undefined;

  // One label for the always-mounted dispatch button: mid-turn sends are
  // queued server-side, so while running it reads "Queue".
  const actionLabel = isRunning ? "Queue" : "Send";
  // Stopping discards the queued sends, so the button says so when any
  // exist. A pending graceful stop outranks both: the label advertises the
  // escalation (pressing again stops immediately).
  const stopLabel = stopPending
    ? "Stop now — stopping after the current tool call"
    : pendingInput > 0
      ? `Stop and discard ${pendingInput} queued message${pendingInput === 1 ? "" : "s"}`
      : "Stop";

  return (
    // The phone face drops the band's border/background and pads out the
    // bottom safe area (mock .cmpw 8/12 + .safeb sabot+10) — the max-lg:
    // overrides win below 1024, desktop keeps the band untouched.
    <div
      hidden={hidden}
      data-testid={phone ? "composer" : undefined}
      className="border-t border-zinc-800 bg-zinc-950 p-3 lg:pr-12 max-lg:border-t-0 max-lg:bg-transparent max-lg:px-3 max-lg:pt-2 max-lg:pb-[calc(env(safe-area-inset-bottom)+18px)]"
    >
      <div className="mx-auto max-w-3xl">
        <div className="relative">
          {/* Kept feature (user decision 1): the watching note. On the phone
              it sits above the pill as a 12px muted line (mock .sub face)
              with a --link "Clear"; on desktop it stays inside the box,
              unchanged. */}
          {phone && watchingRole && (
            <div
              data-testid="composer-watch-note"
              className="flex items-center gap-1.5 px-3 pb-1.5 text-xs text-zinc-500"
            >
              <span>
                🧩 Watching <span className="font-semibold text-zinc-300">{watchingRole}</span>{" "}
                — your message goes to the orchestrator
              </span>
              <button
                onClick={onClearWatch}
                className="ml-auto shrink-0 cursor-pointer text-xs font-semibold text-sky-400"
              >
                Clear
              </button>
            </div>
          )}
          {showPalette && (
            <div className="absolute bottom-full left-0 z-20 mb-2 max-h-72 w-full overflow-y-auto rounded-lg border border-zinc-800 bg-zinc-900 py-1 shadow-xl">
              {matches.map((c, i) => (
                <button
                  key={c.name}
                  // Keep textarea focus so keyboard flow isn't broken.
                  onMouseDown={(e) => {
                    e.preventDefault();
                    accept(c);
                  }}
                  className={`flex w-full items-baseline gap-2 px-3 py-1.5 text-left text-xs ${
                    i === sel ? "bg-zinc-800" : "hover:bg-zinc-800/60"
                  }`}
                >
                  <span className="font-mono font-medium text-emerald-400">
                    /{c.name}
                    {c.takes_args && <span className="text-zinc-500"> &lt;args&gt;</span>}
                  </span>
                  <span className="truncate text-zinc-500">{c.description}</span>
                </button>
              ))}
            </div>
          )}

          {menuOpen && (
            <div
              ref={menuRef}
              role="menu"
              aria-label="Attach"
              data-testid="attach-menu"
              // Phone: the mock's .amenu (s1 surface, bd2 border, r16, soft
              // shadow, 6px padding). Non-phone keeps the shared look.
              className={
                phone
                  ? "absolute bottom-full left-0 z-20 mb-2 w-56 rounded-2xl border border-zinc-700 bg-zinc-900 p-1.5 shadow-[0_12px_32px_rgba(0,0,0,0.45)]"
                  : "absolute bottom-full left-0 z-20 mb-2 w-56 rounded-lg border border-zinc-800 bg-zinc-900 py-1 shadow-xl"
              }
            >
              {ATTACH_ITEMS.map(({ testid, label, ref, icon }) => (
                <button
                  key={testid}
                  type="button"
                  role="menuitem"
                  data-testid={testid}
                  {...keepTextareaFocus}
                  onClick={() => pick(attachInputRefs[ref])}
                  className={
                    phone
                      ? "flex h-11 w-[212px] items-center gap-2.5 rounded-[11px] px-3 text-left text-[14.5px] text-zinc-200 active:bg-zinc-800"
                      : "block w-full px-4 py-3 text-left text-sm text-zinc-200 hover:bg-zinc-800"
                  }
                >
                  {phone && (
                    // .ai — a 24px centred icon box in front of the label.
                    <span className="grid w-6 place-items-center">
                      <svg
                        aria-hidden
                        width="20"
                        height="20"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="1.8"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                      >
                        {icon}
                      </svg>
                    </span>
                  )}
                  {label}
                </button>
              ))}
            </div>
          )}

          {/* Phone face: the mock's single 52px pill (r26, bd2 border, 3px
              padding, soft shadow) — max-lg: only, the desktop box is
              untouched. */}
          <div className="flex flex-col gap-2 rounded-xl border border-zinc-800 bg-zinc-900 p-2 focus-within:border-zinc-700 max-lg:min-h-[52px] max-lg:gap-0 max-lg:rounded-sheet max-lg:border-zinc-700 max-lg:p-[3px] max-lg:shadow-[0_2px_10px_rgba(0,0,0,0.25)] max-lg:light:shadow-[0_2px_10px_rgba(0,0,0,0.06)] max-lg:focus-within:border-zinc-700">
            {/* Inside the box (not the desktop-only hint row below) so it also
                shows on touch: selecting a role filters the transcript, it does
                not re-route input. */}
            {!phone && watchingRole && (
              <div className="flex items-center gap-1.5 px-1 pt-0.5 text-[11px] text-zinc-400">
                {/* Zinc, not sky: the palette's zinc ramp is the one thing light
                    mode remaps, so a sky tint here would be unreadable on the
                    light composer surface. */}
                <span>
                  🧩 Watching <span className="font-medium">{watchingRole}</span> — this
                  is a view. Your message goes to the orchestrator, which decides what
                  to delegate.
                </span>
                <button
                  onClick={onClearWatch}
                  className="shrink-0 cursor-pointer rounded px-1 text-zinc-300 underline hover:bg-zinc-800"
                >
                  Clear
                </button>
              </div>
            )}

            {attachments.pending.length > 0 && (
              <div className="flex flex-wrap items-start gap-2 px-1 pt-1 max-lg:px-2.5 max-lg:pt-2">
                {attachments.pending.map((chip) => (
                  <AttachmentChip
                    key={chip.key}
                    chip={chip}
                    supportsVision={supportsVision}
                    base={base}
                    onRemove={() => attachments.remove(chip.key)}
                  />
                ))}
              </div>
            )}
            {attachments.pending.some(
              (chip) => visionWarning(chip, supportsVision) === NO_VISION,
            ) && (
              <p role="status" className="px-1 pt-1 text-xs text-amber-400">
                {NO_VISION}
              </p>
            )}

            <div className="flex items-end gap-2 max-lg:gap-0.5">
              <input
                ref={fileInputRef}
                type="file"
                multiple
                onChange={onPickFiles}
                className="hidden"
              />
              {android && (
                <input
                  ref={cameraInputRef}
                  type="file"
                  accept="image/*"
                  capture="environment"
                  onChange={onPickFiles}
                  className="hidden"
                />
              )}
              {android && (
                <input
                  ref={galleryInputRef}
                  type="file"
                  accept="image/*"
                  multiple
                  onChange={onPickFiles}
                  className="hidden"
                />
              )}
              <button
                type="button"
                ref={attachButtonRef}
                disabled={!connected}
                onClick={() => (android ? setMenuOpen((o) => !o) : fileInputRef.current?.click())}
                aria-haspopup={android ? "menu" : undefined}
                aria-expanded={android ? menuOpen : undefined}
                {...(android ? keepTextareaFocus : {})}
                title="Attach files"
                aria-label="Attach files"
                className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200 disabled:cursor-not-allowed disabled:opacity-40 max-lg:h-11 max-lg:w-11 max-lg:rounded-full max-lg:text-zinc-400"
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth={phone ? 1.8 : 2}
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  className="h-5 w-5 max-lg:h-6 max-lg:w-6"
                >
                  {phone ? (
                    // The mock's round "+" attach (proto:406-408).
                    <path d="M12 5v14M5 12h14" />
                  ) : (
                    <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
                  )}
                </svg>
              </button>
              <textarea
                ref={textareaRef}
                rows={1}
                disabled={!connected}
                value={text}
                onChange={(e) => onChange(e.target.value)}
                onKeyDown={onKeyDown}
                onPaste={onPaste}
                placeholder={placeholder}
                style={{ minHeight: MIN_H_P, maxHeight: MAX_H_P }}
                className="flex-1 resize-none overflow-y-auto bg-transparent px-2 py-1.5 text-sm text-zinc-100 placeholder-zinc-600 outline-none disabled:cursor-not-allowed max-lg:px-1.5 max-lg:py-[11px] max-lg:text-base max-lg:leading-[22px] max-lg:placeholder-zinc-500"
              />
              {isRunning && !hasContent ? (
                // Empty box mid-turn: icon-only controls — labels would leave
                // ~56px for the textarea on a 360px phone. Queue (below)
                // replaces them the moment there's content to send.
                <>
                  {/* Pause/Resume: only while a pausable sub-agent is
                      running. Explicit frames (pause / resume), never a
                      toggle — the glyph tracks the wire pause state. Amber
                      keeps it distinct from Stop's red (abort everything). */}
                  {subAgent?.pausable && (
                    <button
                      {...keepTextareaFocus}
                      onClick={subAgent.pause === "running" ? onPause : onResume}
                      title={
                        subAgent.pause === "running"
                          ? "Pause sub-agent after its current step"
                          : "Resume sub-agent"
                      }
                      aria-label={subAgent.pause === "running" ? "Pause" : "Resume"}
                      className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-amber-950/70 text-amber-300 hover:bg-amber-900/70 max-lg:m-px max-lg:mr-0.5 max-lg:h-[42px] max-lg:w-[42px] max-lg:rounded-full max-lg:bg-amber-500/14 max-lg:text-amber-400"
                    >
                      {phone ? (
                        // The mock's face: an 18px two-bar icon while running,
                        // a 10.5px "Resume" word while paused (proto:547, 184).
                        subAgent.pause === "running" ? (
                          <svg
                            aria-hidden
                            width="18"
                            height="18"
                            viewBox="0 0 24 24"
                            fill="currentColor"
                          >
                            <rect x="6" y="5" width="4" height="14" rx="1.2" />
                            <rect x="14" y="5" width="4" height="14" rx="1.2" />
                          </svg>
                        ) : (
                          <span className="text-[10.5px] font-bold tracking-[0.02em]">
                            Resume
                          </span>
                        )
                      ) : (
                        <span className="text-xs leading-none">
                          {subAgent.pause === "running" ? "⏸" : "▶"}
                        </span>
                      )}
                    </button>
                  )}
                  <button
                    {...keepTextareaFocus}
                    onClick={onStop}
                    title={stopLabel}
                    aria-label={stopLabel}
                    className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-red-950/70 hover:bg-red-900/70 max-lg:m-px max-lg:h-[42px] max-lg:w-[42px] max-lg:rounded-full max-lg:bg-red-600/18 ${
                      // A graceful stop is armed: a subtle pulse + ring on the
                      // existing red surface says "press again to stop now".
                      stopPending ? "animate-pulse ring-2 ring-red-500/60" : ""
                    }`}
                  >
                    <span className="h-2.5 w-2.5 rounded-[2px] bg-red-400 max-lg:h-3.5 max-lg:w-3.5 max-lg:rounded-[3px] max-lg:bg-red-600" />
                  </button>
                </>
              ) : phone && !hasContent ? null : (
                <button
                  {...keepTextareaFocus}
                  onClick={submit}
                  disabled={!canSend}
                  title={sendBlockedReason}
                  className="rounded-lg bg-emerald-700 px-3.5 py-1.5 text-sm font-medium text-white hover:bg-emerald-600 disabled:opacity-40 max-lg:m-px max-lg:h-[42px] max-lg:shrink-0 max-lg:rounded-[21px] max-lg:bg-emerald-700 max-lg:px-5 max-lg:py-0 max-lg:text-[15px] max-lg:font-semibold"
                >
                  {actionLabel}
                </button>
              )}
            </div>
          </div>
        </div>
        {/* Always visible (unlike the lg hint row below): on touch there is no
            hover/focus, so the discard warning can't ride Stop's aria-label. */}
        {isRunning && pendingInput > 0 && (
          <div className="mt-1.5 px-1 text-[11px] text-zinc-600">
            ⏳ {pendingInput} queued · sent when this turn ends · Stop discards them
          </div>
        )}
        {/* Hints are desktop-only — on touch screens they eat vertical real
            estate under the composer for no benefit. */}
        <div className="mt-1.5 hidden flex-wrap items-center gap-x-3 gap-y-1 px-1 text-[11px] text-zinc-600 lg:flex">
          {touchInput ? (
            <span>
              <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">{actionLabel}</kbd> to dispatch
            </span>
          ) : (
            <span>
              <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">Enter</kbd> to {actionLabel.toLowerCase()}
            </span>
          )}
          <span>
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">/</kbd> for commands
          </span>
          {settingsEnabled && (
            <span data-testid="composer-settings-hint">
              <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">Ctrl+,</kbd> settings
            </span>
          )}
          <span>
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">Shift+Enter</kbd> newline
          </span>
          <span>
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">📎</kbd> or{" "}
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">paste</kbd> /{" "}
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">drag</kbd> files
          </span>
        </div>
      </div>
    </div>
  );
}
