// @vitest-environment jsdom
//
// App-level tests for per-tab composers (desktop-tab-composers, keep-alive).
// Written BEFORE the implementation, against the locked design:
//
//   - Desktop (`initialTabs !== null`): App renders one <ComposerSlot
//     key={tab.id}> per tab. The active slot gets `live`; the others are
//     parked + hidden but stay MOUNTED, so each tab keeps its own text,
//     attachments (incl. in-flight uploads) and slash-palette state.
//   - Web (`initialTabs === null`): exactly one slot, convo =
//     state.conversation.id, unchanged behaviour.
//   - The drop target (DropOverlay) feeds the ACTIVE slot only
//     (`activeSlot.current?.add(files)`). App no longer calls useAttachments.
//
// Harness: the REAL App tree, with
//   - `./useAgent` mocked by a controllable fake agent. Like the real hook it
//     only changes `convo`/`state` when the test says so ("the server bound
//     it"), so `switchConvo` is a plain spy.
//   - `./uploads` mocked: every `uploadFile` is a deferred promise the test
//     resolves by hand, with an abort spy.
// Tab fixtures carry literal ids (the ConvoTab shape is `{id, convo, title}`),
// so this file doesn't depend on `freshTab`.
//
// Finding things in the DOM (deliberately via public, user-visible hooks):
//   - conversation tabs : `nav[aria-label="Conversations"] [role="tab"]`
//                         (the right-hand drawer also uses role="tab")
//   - close tab         : button[aria-label="Close tab"] inside a tab
//   - composers         : every <textarea>; a composer is "visible" iff no
//                         ancestor carries the `hidden` attribute
//   - chips             : button[aria-label="Remove <file name>"]
//   - upload state      : the composer's Send button — `title` is
//                         "Waiting for uploads to finish" while any chip is
//                         uploading, and it is enabled once the chip is done.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import { uploadFile } from "./uploads";
import type { AppState, InboundMessage, WireAttachment } from "./state";
import type { TabsState } from "./convoTabs";

// ─── mocks ───────────────────────────────────────────────────────────────

const hoisted = vi.hoisted(() => ({ agent: null as unknown }));

// `useConvoTabs` also imports `setConvoInUrl` from "./useAgent".
vi.mock("./useAgent", () => ({
  useAgent: () => hoisted.agent,
  setConvoInUrl: vi.fn(),
}));
vi.mock("./uploads", () => ({ uploadFile: vi.fn() }));

const uploadFileMock = vi.mocked(uploadFile);

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fake agent ──────────────────────────────────────────────────────────

interface FakeAgent {
  connected: boolean;
  state: AppState | null;
  models: unknown[];
  activeAlias: string;
  conversations: unknown[];
  conversationsError: string | null;
  commands: unknown[];
  dirListing: null;
  recentDirs: string[];
  error: string | null;
  send: ReturnType<typeof vi.fn<(msg: InboundMessage) => void>>;
  convo: string | null;
  switchConvo: ReturnType<typeof vi.fn<(id: string | null) => void>>;
}

/** A minimal, valid-enough `AppState` frame for conversation `id`. */
function stateFor(id: string | null, over: Partial<AppState> = {}): AppState {
  return {
    chat: { messages: [], auto_scroll: true, scroll_offset: 0 },
    todo: { visible: false, items: [] },
    stats: {
      total_input_tokens: 0,
      total_output_tokens: 0,
      total_api_calls: 0,
      total_cost: 0,
      model: "m",
      provider_name: "p",
      model_alias: "a",
    },
    context: {
      current_usage: 0,
      window_size: 1000,
      compaction_enabled: true,
      compaction_threshold: 0.8,
    },
    conversation: id === null ? null : { id },
    is_running: false,
    is_loading: false,
    welcome: null,
    exit_requested: false,
    bg: { running_count: 0, recent_summaries: [] },
    bash_panel: { kind: "idle" },
    ...over,
  } as unknown as AppState;
}

function makeAgent(convo: string | null): FakeAgent {
  return {
    connected: true,
    state: stateFor(convo),
    models: [],
    activeAlias: "",
    conversations: [],
    conversationsError: null,
    commands: [],
    dirListing: null,
    recentDirs: [],
    error: null,
    send: vi.fn(),
    convo,
    switchConvo: vi.fn(),
  };
}

let agent: FakeAgent;

/** The server confirmed the socket is now on `convo` (what useAgent's
 * `attached` / `state` frames do). */
function bindServer(convo: string | null): void {
  agent.convo = convo;
  agent.state = stateFor(convo);
}

// ─── mocked uploads ──────────────────────────────────────────────────────

interface Upload {
  convo: string;
  file: File;
  abort: ReturnType<typeof vi.fn>;
  resolve: (a: WireAttachment) => void;
}
let uploads: Upload[] = [];

function attachment(id: string, name: string, convo: string): WireAttachment {
  return { id, convo, name, mime: "application/pdf", size: 1234, kind: "file" };
}

// ─── render harness ──────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;
let currentTabs: TabsState | null = null;
let originalMatchMedia: typeof window.matchMedia | undefined;

beforeEach(() => {
  originalMatchMedia = window.matchMedia;
  window.matchMedia = ((media: string) =>
    ({
      matches: false,
      media,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList) as typeof window.matchMedia;

  localStorage.clear();
  uploads = [];
  uploadFileMock.mockReset();
  uploadFileMock.mockImplementation((convo, file) => {
    const abort = vi.fn();
    let resolve!: (a: WireAttachment) => void;
    const done = new Promise<WireAttachment>((res) => {
      resolve = res;
    });
    uploads.push({ convo, file, abort, resolve });
    return { done, abort };
  });
  let previews = 0;
  URL.createObjectURL = vi.fn(() => `blob:p${(previews += 1)}`);
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  hoisted.agent = null;
  if (originalMatchMedia === undefined) {
    delete (window as unknown as { matchMedia?: unknown }).matchMedia;
  } else {
    window.matchMedia = originalMatchMedia;
  }
  delete (URL as unknown as Record<string, unknown>)["createObjectURL"];
  delete (URL as unknown as Record<string, unknown>)["revokeObjectURL"];
});

const tab = (id: string, convo: string | null, title: string) => ({ id, convo, title });
const tabsState = (tabs: ReturnType<typeof tab>[], active = 0): TabsState =>
  ({ tabs, active }) as unknown as TabsState;

async function mountApp(initialTabs: TabsState | null, forConvo: string | null): Promise<void> {
  agent = makeAgent(forConvo);
  hoisted.agent = agent;
  currentTabs = initialTabs;
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<App initialTabs={initialTabs} />);
  });
}

/** Re-render App (after mutating the fake agent), preserving its state. */
async function rerenderApp(): Promise<void> {
  await act(async () => {
    root!.render(<App initialTabs={currentTabs} />);
  });
}

// ─── DOM helpers ─────────────────────────────────────────────────────────

const tabEls = (): HTMLElement[] =>
  Array.from(
    document.querySelectorAll<HTMLElement>('nav[aria-label="Conversations"] [role="tab"]'),
  );

const allTextareas = (): HTMLTextAreaElement[] =>
  Array.from(document.querySelectorAll("textarea"));

const visibleTextareas = (): HTMLTextAreaElement[] =>
  allTextareas().filter((ta) => ta.closest("[hidden]") === null);

const hiddenTextareas = (): HTMLTextAreaElement[] =>
  allTextareas().filter((ta) => ta.closest("[hidden]") !== null);

/** The `i`-th parked (hidden) textbox, failing with a clear message if the
 * per-tab composers aren't there (today there is only one shared composer). */
function hiddenBox(i = 0): HTMLTextAreaElement {
  const h = hiddenTextareas();
  expect(h.length, `expected a hidden (parked) composer #${i}; found ${h.length}`).toBeGreaterThan(i);
  return h[i];
}

/** The one visible textbox (fails loudly if there isn't exactly one). */
function theVisibleTextbox(): HTMLTextAreaElement {
  const v = visibleTextareas();
  expect(v, "exactly one composer textbox must be visible").toHaveLength(1);
  return v[0];
}

/** The composer subtree around `ta`: the largest ancestor holding no other textarea. */
function composerOf(ta: HTMLTextAreaElement): HTMLElement {
  let n: HTMLElement = ta;
  while (n.parentElement && n.parentElement.querySelectorAll("textarea").length === 1) {
    n = n.parentElement;
  }
  return n;
}

const chipOf = (scope: HTMLElement, name: string): Element | null =>
  scope.querySelector(`button[aria-label="Remove ${name}"]`);

const sendButtonOf = (scope: HTMLElement): HTMLButtonElement | undefined =>
  Array.from(scope.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").trim() === "Send",
  );

const isUploading = (scope: HTMLElement): boolean =>
  (sendButtonOf(scope)?.title ?? "").includes("Waiting for uploads");

const textareaSetter = Object.getOwnPropertyDescriptor(
  HTMLTextAreaElement.prototype,
  "value",
)!.set!;

async function typeInto(ta: HTMLTextAreaElement, value: string): Promise<void> {
  await act(async () => {
    textareaSetter.call(ta, value);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function pressEnter(ta: HTMLTextAreaElement): Promise<void> {
  await act(async () => {
    ta.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );
  });
}

const pdf = (name: string): File => new File(["pdf-bytes"], name, { type: "application/pdf" });

/** Attach through the composer's own file input (what the 📎 button opens). */
async function attachViaInput(scope: HTMLElement, file: File): Promise<void> {
  const input = scope.querySelector<HTMLInputElement>('input[type="file"]');
  expect(input, "composer must render its file input").not.toBeNull();
  Object.defineProperty(input, "files", { value: [file], configurable: true });
  await act(async () => {
    input!.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

function dragEvent(type: string, dataTransfer: Record<string, unknown>): Event {
  const ev = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(ev, "dataTransfer", { value: dataTransfer });
  return ev;
}

/** Drop `files` on the chat's drop target (DropOverlay's <main>). */
async function dropFiles(...files: File[]): Promise<void> {
  const main = document.querySelector("main");
  expect(main, "the DropOverlay <main> must exist").not.toBeNull();
  await act(async () => {
    main!.dispatchEvent(dragEvent("dragenter", { types: ["Files"] }));
  });
  await act(async () => {
    main!.dispatchEvent(dragEvent("drop", { types: ["Files"], files }));
  });
}

async function clickTab(index: number): Promise<void> {
  const el = tabEls()[index];
  expect(el, `tab #${index} must exist`).toBeDefined();
  await act(async () => {
    el.click();
  });
}

async function closeTabAt(index: number): Promise<void> {
  const x = tabEls()[index]?.querySelector<HTMLButtonElement>('button[aria-label="Close tab"]');
  expect(x, `tab #${index} must have a close button`).toBeTruthy();
  await act(async () => {
    x!.click();
  });
}

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

/** Click tab `index`, then simulate the server binding `convo` and re-render. */
async function switchTo(index: number, convo: string | null): Promise<void> {
  await clickTab(index);
  bindServer(convo);
  await rerenderApp();
}

const sentMessages = (): InboundMessage[] =>
  agent.send.mock.calls
    .map((c) => c[0])
    .filter((m): m is Extract<InboundMessage, { type: "send_message" }> => m.type === "send_message");

// ═════════════════════════════════════════════════════════════════════════
// Desktop: one composer per tab
// ═════════════════════════════════════════════════════════════════════════

describe("App (desktop) — one keep-alive composer per tab", () => {
  it("mounts one composer per tab and shows exactly one of them", async () => {
    await mountApp(
      tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B"), tab("t-c", "c", "C")], 1),
      "b",
    );
    expect(tabEls()).toHaveLength(3);
    expect(allTextareas(), "one <textarea> per tab").toHaveLength(3);
    expect(visibleTextareas()).toHaveLength(1);
    expect(hiddenTextareas()).toHaveLength(2);
  });

  it("the visible composer is the active tab's (switching moves visibility, not content)", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 0), "a");
    expect(allTextareas(), "one <textarea> per tab").toHaveLength(2);
    const [first, second] = allTextareas();
    expect(first.closest("[hidden]")).toBeNull();
    expect(second.closest("[hidden]")).not.toBeNull();

    await switchTo(1, "b");

    expect(allTextareas()[0]).toBe(first); // same nodes: nothing remounted
    expect(allTextareas()[1]).toBe(second);
    expect(first.closest("[hidden]")).not.toBeNull();
    expect(second.closest("[hidden]")).toBeNull();
  });

  it("a new tab (+) adds a fresh, empty, visible composer and parks the old one with its draft", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A")], 0), "a");
    await typeInto(theVisibleTextbox(), "draft in A");

    const plus = document.querySelector<HTMLButtonElement>('button[aria-label="New conversation tab"]')!;
    await act(async () => {
      plus.click();
    });
    bindServer(null);
    await rerenderApp();

    expect(allTextareas()).toHaveLength(2);
    expect(theVisibleTextbox().value).toBe("");
    expect(hiddenBox().value).toBe("draft in A");
  });

  it("a new tab's composer survives the server binding its conversation (null → 'u'): same node, same draft", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-new", null, "")], 1), null);
    const ta = theVisibleTextbox();
    await typeInto(ta, "typed before the server answered");

    bindServer("u");
    await rerenderApp();

    expect(allTextareas(), "binding must not add or drop a composer").toHaveLength(2);
    expect(theVisibleTextbox()).toBe(ta);
    expect(ta.value).toBe("typed before the server answered");
  });

  it("a hidden tab's composer is not interactive: the visible one is the only one that can send", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 0), "a");
    await typeInto(theVisibleTextbox(), "to A");
    await switchTo(1, "b");

    // Hidden A still holds its text but pressing Enter there must not send.
    const hiddenA = hiddenBox();
    expect(hiddenA.value).toBe("to A");
    await pressEnter(hiddenA);
    expect(sentMessages()).toHaveLength(0);
  });
});

// ═════════════════════════════════════════════════════════════════════════
// The headline scenario
// ═════════════════════════════════════════════════════════════════════════

describe("App (desktop) — each tab keeps its own text and attachments", () => {
  it("A and B keep independent drafts and uploads; A's upload finishes in the background and sends from A", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 0), "a");

    // 2. In A: type and attach fa.
    const fa = pdf("fa.pdf");
    await typeInto(theVisibleTextbox(), "hello A");
    await attachViaInput(composerOf(theVisibleTextbox()), fa);
    expect(uploadFileMock).toHaveBeenCalledTimes(1);
    expect(uploadFileMock).toHaveBeenCalledWith("a", fa, expect.any(Function));
    expect(chipOf(composerOf(theVisibleTextbox()), "fa.pdf")).not.toBeNull();

    // 3. Click tab B; the app asks to switch, then the server binds "b".
    await clickTab(1);
    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith("b");
    bindServer("b");
    await rerenderApp();

    // 4. B's composer starts empty; type and attach fb under "b".
    expect(theVisibleTextbox().value, "B must not inherit A's draft").toBe("");
    expect(chipOf(composerOf(theVisibleTextbox()), "fa.pdf"), "B must not show A's chip").toBeNull();
    const fb = pdf("fb.pdf");
    await typeInto(theVisibleTextbox(), "hello B");
    await attachViaInput(composerOf(theVisibleTextbox()), fb);
    expect(uploadFileMock).toHaveBeenCalledTimes(2);
    expect(uploadFileMock).toHaveBeenLastCalledWith("b", fb, expect.any(Function));

    // A's upload was NOT aborted by switching away.
    expect(uploads[0].abort).not.toHaveBeenCalled();

    // 5. fa's upload completes while B is the active tab.
    await act(async () => {
      uploads[0].resolve(attachment("att-fa", "fa.pdf", "a"));
    });
    await flush();

    // 6. Back to A: its draft and its (now done) chip are there.
    await switchTo(0, "a");
    const aBox = theVisibleTextbox();
    expect(aBox.value).toBe("hello A");
    const aComposer = composerOf(aBox);
    expect(chipOf(aComposer, "fa.pdf")).not.toBeNull();
    expect(isUploading(aComposer), "fa finished in the background: chip is done").toBe(false);
    expect(sendButtonOf(aComposer)!.disabled).toBe(false);

    // …and B's hidden composer still holds its own draft with fb still uploading.
    const bBox = hiddenBox();
    expect(bBox.value).toBe("hello B");
    const bComposer = composerOf(bBox);
    expect(chipOf(bComposer, "fb.pdf")).not.toBeNull();
    expect(chipOf(bComposer, "fa.pdf")).toBeNull();
    expect(isUploading(bComposer), "fb is still in flight").toBe(true);
    expect(uploads[1].abort).not.toHaveBeenCalled();

    // 7. Enter in A sends A's text with A's attachment id — nothing of B's.
    await pressEnter(aBox);
    expect(sentMessages()).toEqual([
      { type: "send_message", text: "hello A", attachments: ["att-fa"] },
    ]);

    // Sending clears A only; B is untouched.
    expect(aBox.value).toBe("");
    expect(chipOf(aComposer, "fa.pdf")).toBeNull();
    expect(bBox.value).toBe("hello B");
    expect(chipOf(bComposer, "fb.pdf")).not.toBeNull();
  });

  it("B's upload can complete while B is hidden, and is then sent from B with B's id", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 1), "b");
    await typeInto(theVisibleTextbox(), "hello B");
    await attachViaInput(composerOf(theVisibleTextbox()), pdf("fb.pdf"));
    expect(uploads[0].convo).toBe("b");

    await switchTo(0, "a");
    await act(async () => {
      uploads[0].resolve(attachment("att-fb", "fb.pdf", "b"));
    });
    await flush();
    await switchTo(1, "b");

    const box = theVisibleTextbox();
    expect(box.value).toBe("hello B");
    expect(isUploading(composerOf(box))).toBe(false);
    await pressEnter(box);
    expect(sentMessages()).toEqual([
      { type: "send_message", text: "hello B", attachments: ["att-fb"] },
    ]);
  });

  it("each tab keeps its own slash-palette state (a dismissed palette stays dismissed on return)", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 0), "a");
    agent.commands = [
      { name: "help", description: "Show help", takes_args: false },
      { name: "history", description: "Show history", takes_args: false },
    ];
    await rerenderApp();

    const palette = (): number =>
      Array.from(document.querySelectorAll("button")).filter((b) =>
        (b.textContent ?? "").trim().startsWith("/h"),
      ).length;

    const aBox = theVisibleTextbox();
    await typeInto(aBox, "/h");
    expect(palette(), "palette open in A").toBe(2);
    await act(async () => {
      aBox.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
      );
    });
    expect(palette(), "Esc dismisses it").toBe(0);

    await switchTo(1, "b");
    expect(theVisibleTextbox().value).toBe("");
    expect(palette(), "B has no '/' text, so no palette").toBe(0);

    await switchTo(0, "a");
    expect(theVisibleTextbox().value).toBe("/h");
    expect(palette(), "A's dismissal is remembered").toBe(0);
  });

  it("the active composer is wired to live state: a running turn shows Stop in the active tab only", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 0), "a");
    agent.state = stateFor("a", { is_running: true });
    await rerenderApp();

    const stops = (): number =>
      document.querySelectorAll('button[aria-label^="Stop"]').length;
    expect(stops(), "only the active tab's composer shows Stop").toBe(1);
    expect(composerOf(theVisibleTextbox()).querySelector('button[aria-label^="Stop"]')).not.toBeNull();
  });
});

// ═════════════════════════════════════════════════════════════════════════
// Drag & drop goes to the ACTIVE tab only
// ═════════════════════════════════════════════════════════════════════════

describe("App (desktop) — dropping files targets the active tab", () => {
  it("uploads under the active tab's convo and shows the chip only in that tab", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 0), "a");
    const f1 = pdf("dropped-a.pdf");
    await dropFiles(f1);

    expect(uploadFileMock).toHaveBeenCalledTimes(1);
    expect(uploadFileMock).toHaveBeenCalledWith("a", f1, expect.any(Function));
    expect(chipOf(composerOf(theVisibleTextbox()), "dropped-a.pdf")).not.toBeNull();
    expect(chipOf(composerOf(hiddenBox()), "dropped-a.pdf")).toBeNull();
  });

  it("after switching tabs, a drop goes to the NEW active tab and the old tab keeps its earlier drop", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 0), "a");
    const fa = pdf("for-a.pdf");
    const fb = pdf("for-b.pdf");

    await dropFiles(fa);
    await switchTo(1, "b");
    await dropFiles(fb);

    expect(uploads.map((u) => [u.convo, u.file.name])).toEqual([
      ["a", "for-a.pdf"],
      ["b", "for-b.pdf"],
    ]);
    const bComposer = composerOf(theVisibleTextbox());
    expect(chipOf(bComposer, "for-b.pdf")).not.toBeNull();
    expect(chipOf(bComposer, "for-a.pdf"), "B must not show A's file").toBeNull();
    const aComposer = composerOf(hiddenBox());
    expect(chipOf(aComposer, "for-a.pdf"), "A keeps its chip while hidden").not.toBeNull();
    expect(chipOf(aComposer, "for-b.pdf")).toBeNull();
    expect(uploads[0].abort, "switching tabs must not cancel A's upload").not.toHaveBeenCalled();
  });

  it("dropping several files uploads each under the active convo", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 1), "b");
    await dropFiles(pdf("1.pdf"), pdf("2.pdf"));
    expect(uploads.map((u) => u.convo)).toEqual(["b", "b"]);
  });
});

// ═════════════════════════════════════════════════════════════════════════
// Closing tabs
// ═════════════════════════════════════════════════════════════════════════

describe("App (desktop) — closing a tab", () => {
  it("closing tab B mid-upload aborts fb and revokes its preview", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 0), "a");
    await switchTo(1, "b");
    await typeInto(theVisibleTextbox(), "hello B");
    const png = new File(["png"], "fb.png", { type: "image/png" });
    await attachViaInput(composerOf(theVisibleTextbox()), png);
    expect(uploads[0].convo).toBe("b");
    await switchTo(0, "a");
    expect(uploads[0].abort).not.toHaveBeenCalled();

    await closeTabAt(1);

    expect(uploads[0].abort).toHaveBeenCalledTimes(1);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:p1");
    expect(tabEls()).toHaveLength(1);
    expect(allTextareas(), "B's composer is unmounted with its tab").toHaveLength(1);
  });

  it("closing a tab does not disturb the other tabs' drafts or uploads", async () => {
    await mountApp(
      tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B"), tab("t-c", "c", "C")], 0),
      "a",
    );
    await typeInto(theVisibleTextbox(), "draft A");
    await attachViaInput(composerOf(theVisibleTextbox()), pdf("fa.pdf"));
    await switchTo(2, "c");
    await typeInto(theVisibleTextbox(), "draft C");

    await closeTabAt(1); // B (inactive, middle)

    expect(allTextareas().map((t) => t.value).sort()).toEqual(["draft A", "draft C"]);
    expect(theVisibleTextbox().value).toBe("draft C");
    expect(uploads[0].abort, "A's upload is unaffected").not.toHaveBeenCalled();
    expect(chipOf(composerOf(hiddenBox()), "fa.pdf")).not.toBeNull();
  });

  it("closing the ACTIVE tab removes its composer and activates the neighbour with its own draft intact", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 1), "b");
    await typeInto(theVisibleTextbox(), "draft B");
    await switchTo(0, "a");
    await typeInto(theVisibleTextbox(), "draft A");

    await closeTabAt(0); // close A (active) → B becomes active
    expect(agent.switchConvo).toHaveBeenLastCalledWith("b");
    bindServer("b");
    await rerenderApp();

    expect(allTextareas()).toHaveLength(1);
    expect(theVisibleTextbox().value).toBe("draft B");
  });

  it("closing the LAST tab leaves one fresh, empty composer (the closed tab's draft is gone)", async () => {
    await mountApp(tabsState([tab("t-a", "a", "A")], 0), "a");
    await typeInto(theVisibleTextbox(), "doomed draft");
    await attachViaInput(composerOf(theVisibleTextbox()), pdf("doomed.pdf"));

    await closeTabAt(0);
    bindServer(null);
    await rerenderApp();

    expect(uploads[0].abort).toHaveBeenCalledTimes(1);
    expect(allTextareas()).toHaveLength(1);
    expect(theVisibleTextbox().value).toBe("");
    expect(chipOf(composerOf(theVisibleTextbox()), "doomed.pdf")).toBeNull();
  });
});

// ═════════════════════════════════════════════════════════════════════════
// Web build: unchanged
// ═════════════════════════════════════════════════════════════════════════

describe("App (web, initialTabs = null) — unchanged single composer", () => {
  it("renders exactly one textbox, visible, and no conversation tab bar", async () => {
    await mountApp(null, "w1");
    expect(allTextareas()).toHaveLength(1);
    expect(visibleTextareas()).toHaveLength(1);
    expect(tabEls()).toHaveLength(0);
  });

  it("uploads under state.conversation.id (file input)", async () => {
    await mountApp(null, "w1");
    const f = pdf("web.pdf");
    await attachViaInput(composerOf(theVisibleTextbox()), f);
    expect(uploadFileMock).toHaveBeenCalledTimes(1);
    expect(uploadFileMock).toHaveBeenCalledWith("w1", f, expect.any(Function));
  });

  it("uploads dropped files under state.conversation.id", async () => {
    await mountApp(null, "w1");
    const f = pdf("dropped.pdf");
    await dropFiles(f);
    expect(uploadFileMock).toHaveBeenCalledWith("w1", f, expect.any(Function));
    expect(chipOf(composerOf(theVisibleTextbox()), "dropped.pdf")).not.toBeNull();
  });

  it("before the first state frame (no conversation) an add is rejected, not uploaded", async () => {
    await mountApp(null, null);
    agent.state = null;
    await rerenderApp();
    await attachViaInput(composerOf(theVisibleTextbox()), pdf("early.pdf"));
    expect(uploadFileMock).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("Not connected");
  });

  it("a conversation change (e.g. /new) resets the chips, aborts in-flight uploads, keeps the typed text", async () => {
    await mountApp(null, "w1");
    await typeInto(theVisibleTextbox(), "keep this");
    await attachViaInput(composerOf(theVisibleTextbox()), pdf("w.pdf"));

    bindServer("w2");
    await rerenderApp();

    expect(allTextareas()).toHaveLength(1);
    expect(theVisibleTextbox().value).toBe("keep this");
    expect(chipOf(composerOf(theVisibleTextbox()), "w.pdf")).toBeNull();
    expect(uploads[0].abort).toHaveBeenCalledTimes(1);
  });

  it("Enter sends {type:'send_message', text, attachments} for a finished upload", async () => {
    await mountApp(null, "w1");
    await typeInto(theVisibleTextbox(), "look");
    await attachViaInput(composerOf(theVisibleTextbox()), pdf("w.pdf"));
    await act(async () => {
      uploads[0].resolve(attachment("att-w", "w.pdf", "w1"));
    });
    await flush();
    await pressEnter(theVisibleTextbox());
    expect(sentMessages()).toEqual([
      { type: "send_message", text: "look", attachments: ["att-w"] },
    ]);
  });
});
