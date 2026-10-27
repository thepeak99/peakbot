// Tests for `ComposerSlot` — one keep-alive composer per desktop tab
// (desktop-tab-composers). Written BEFORE the implementation, against the
// Architect's locked contract:
//
//   export type ComposerLive = Omit<ComposerProps, "attachments" | "hidden">
//                              & { uploadLimits: UploadLimits };
//   export interface ComposerSlotHandle { add(files: File[]): void }
//   export const ComposerSlot = memo(function ComposerSlot(
//     { convo, live, ref }: { convo: string | null; live: ComposerLive | null;
//                             ref?: Ref<ComposerSlotHandle> }) {...});
//
//   - The slot OWNS one `useAttachments(convo, uploadLimits)`.
//   - `live === null` = inactive: renders `<Composer hidden>` with PARKED
//     props (disconnected, idle, no commands, no-op handlers, default limits).
//     It stays MOUNTED, so text, chips and in-flight uploads survive.
//   - `useImperativeHandle` exposes `{ add: attachments.add }`.
//   - `convo` changing (null→"u", "x"→"y") resets the chips (uploads are
//     per-conversation) but keeps the typed text.
//
// `./uploads` is mocked (no XHR); every upload is a deferred promise the test
// resolves by hand. The stub `ComposerSlot.tsx` currently renders `null`, so
// everything below fails on BEHAVIOUR (no textarea), not on a bad import.
//
// Harness: createRoot + act + matchMedia stub, as in Composer.test.tsx.
// jsdom is automatic for src/components/**.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createRef, type RefObject } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ComposerSlot, type ComposerLive, type ComposerSlotHandle } from "./ComposerSlot";
import { uploadFile } from "../uploads";
import type { WireAttachment } from "../state";

vi.mock("../uploads", () => ({ uploadFile: vi.fn() }));
const uploadFileMock = vi.mocked(uploadFile);

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── matchMedia stub (useMediaQuery calls it on mount) ───────────────────

let originalMatchMedia: typeof window.matchMedia | undefined;

// ─── mocked uploadFile control ───────────────────────────────────────────

interface Upload {
  convo: string;
  file: File;
  abort: ReturnType<typeof vi.fn>;
  resolve: (a: WireAttachment) => void;
  reject: (e: Error) => void;
}
let uploads: Upload[] = [];

let previewSeq = 0;

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

  uploads = [];
  previewSeq = 0;
  uploadFileMock.mockReset();
  uploadFileMock.mockImplementation((convo, file) => {
    const abort = vi.fn();
    let resolve!: (a: WireAttachment) => void;
    let reject!: (e: Error) => void;
    const done = new Promise<WireAttachment>((res, rej) => {
      resolve = res;
      reject = rej;
    });
    uploads.push({ convo, file, abort, resolve, reject });
    return { done, abort };
  });

  URL.createObjectURL = vi.fn(() => `blob:preview-${(previewSeq += 1)}`);
  URL.revokeObjectURL = vi.fn();
});

let container: HTMLDivElement | null = null;
let root: Root | null = null;

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  if (originalMatchMedia === undefined) {
    delete (window as unknown as { matchMedia?: unknown }).matchMedia;
  } else {
    window.matchMedia = originalMatchMedia;
  }
  delete (URL as unknown as Record<string, unknown>)["createObjectURL"];
  delete (URL as unknown as Record<string, unknown>)["revokeObjectURL"];
});

// ─── fixtures ────────────────────────────────────────────────────────────

const LIMITS = { max_file_mb: 50, max_files: 10 };

function makeLive(over: Partial<ComposerLive> = {}): ComposerLive {
  return {
    isRunning: false,
    connected: true,
    commands: [],
    onSend: vi.fn(),
    onStop: vi.fn(),
    subAgent: null,
    onPause: vi.fn(),
    onResume: vi.fn(),
    watchingRole: null,
    onClearWatch: vi.fn(),
    pendingInput: 0,
    supportsVision: true,
    settingsEnabled: true,
    stopPending: false,
    uploadLimits: LIMITS,
    ...over,
  };
}

const pdf = (name: string): File => new File(["pdf-bytes"], name, { type: "application/pdf" });
const png = (name: string): File => new File(["png-bytes"], name, { type: "image/png" });

let attSeq = 0;
function attachment(name: string, convo = "c1"): WireAttachment {
  attSeq += 1;
  return {
    id: `att-${attSeq}`,
    convo,
    name,
    mime: name.endsWith(".png") ? "image/png" : "application/pdf",
    size: 1234,
    kind: name.endsWith(".png") ? "image" : "file",
  };
}

// ─── mount / render helpers ──────────────────────────────────────────────

type SlotProps = {
  convo: string | null;
  live: ComposerLive | null;
  handle?: RefObject<ComposerSlotHandle | null>;
};

const element = ({ convo, live, handle }: SlotProps) => (
  <ComposerSlot convo={convo} live={live} ref={handle} />
);

async function mount(props: SlotProps): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(element(props));
  });
  return container;
}

async function rerender(props: SlotProps): Promise<void> {
  await act(async () => {
    root!.render(element(props));
  });
}

async function unmount(): Promise<void> {
  await act(async () => {
    root!.unmount();
  });
  root = null;
}

/** The slot's textarea; a clear failure if the slot renders nothing. */
function textarea(el: HTMLElement): HTMLTextAreaElement {
  const ta = el.querySelector("textarea");
  expect(ta, "ComposerSlot must render a <Composer> (a textarea), even when inactive").not.toBeNull();
  return ta as HTMLTextAreaElement;
}

/** True when the element or any ancestor below `el` carries `hidden`. */
function isHidden(el: HTMLElement, node: Element): boolean {
  for (let n: Element | null = node; n && n !== el.parentElement; n = n.parentElement) {
    if (n.hasAttribute("hidden")) return true;
  }
  return false;
}

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

async function add(handle: RefObject<ComposerSlotHandle | null>, files: File[]): Promise<void> {
  expect(handle.current, "ref must expose { add }").not.toBeNull();
  await act(async () => {
    handle.current!.add(files);
  });
}

/** Flush a resolved upload promise + the React update it schedules. */
async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

const buttons = (el: HTMLElement): HTMLButtonElement[] =>
  Array.from(el.querySelectorAll("button"));
const sendButton = (el: HTMLElement): HTMLButtonElement | undefined =>
  buttons(el).find((b) => (b.textContent ?? "").trim() === "Send");
const stopButton = (el: HTMLElement): HTMLButtonElement | undefined =>
  buttons(el).find((b) => (b.getAttribute("aria-label") ?? "").startsWith("Stop"));
/** The ✕ button of the chip for `name` (exists iff that chip is rendered). */
const removeChip = (el: HTMLElement, name: string): Element | null =>
  el.querySelector(`[aria-label="Remove ${name}"]`);

// ─── the memo contract ───────────────────────────────────────────────────

describe("ComposerSlot — shape", () => {
  it("is a React.memo component (inactive slots must not re-render on unrelated App updates)", () => {
    expect((ComposerSlot as unknown as { $$typeof: symbol }).$$typeof).toBe(
      Symbol.for("react.memo"),
    );
  });
});

// ─── active / inactive rendering ─────────────────────────────────────────

describe("ComposerSlot — live vs parked rendering", () => {
  it("live=null renders a HIDDEN composer", async () => {
    const el = await mount({ convo: "c1", live: null });
    const ta = textarea(el);
    expect(isHidden(el, ta), "an inactive slot's composer must carry the hidden attribute").toBe(true);
  });

  it("a live object renders a VISIBLE composer", async () => {
    const el = await mount({ convo: "c1", live: makeLive() });
    const ta = textarea(el);
    expect(isHidden(el, ta)).toBe(false);
  });

  it("flips hidden ↔ visible without remounting the textarea", async () => {
    const el = await mount({ convo: "c1", live: makeLive() });
    const ta = textarea(el);
    await rerender({ convo: "c1", live: null });
    expect(textarea(el)).toBe(ta);
    expect(isHidden(el, ta)).toBe(true);
    await rerender({ convo: "c1", live: makeLive() });
    expect(textarea(el)).toBe(ta);
    expect(isHidden(el, ta)).toBe(false);
  });

  it("a live slot is connected and usable (textarea enabled) when live.connected is true", async () => {
    const el = await mount({ convo: "c1", live: makeLive({ connected: true }) });
    expect(textarea(el).disabled).toBe(false);
  });

  it("a live slot passes live.connected=false through (textarea disabled)", async () => {
    const el = await mount({ convo: "c1", live: makeLive({ connected: false }) });
    expect(textarea(el).disabled).toBe(true);
  });

  it("a parked slot is DISCONNECTED regardless of what it last rendered live (textarea disabled)", async () => {
    const el = await mount({ convo: "c1", live: makeLive({ connected: true }) });
    expect(textarea(el).disabled).toBe(false);
    await rerender({ convo: "c1", live: null });
    expect(textarea(el).disabled, "parked props are disconnected").toBe(true);
  });

  it("a parked slot is IDLE: the live run state (Stop control) does not leak into it", async () => {
    const el = await mount({ convo: "c1", live: makeLive({ isRunning: true }) });
    expect(stopButton(el), "live+running shows Stop").toBeDefined();
    await rerender({ convo: "c1", live: null });
    expect(stopButton(el), "a parked composer must not show Stop").toBeUndefined();
  });

  it("a parked slot drops live.commands: no slash palette even for a matching '/' prefix", async () => {
    const commands = [{ name: "help", description: "Show help", takes_args: false }];
    const el = await mount({ convo: "c1", live: makeLive({ commands }) });
    await typeInto(textarea(el), "/he");
    expect(buttons(el).some((b) => (b.textContent ?? "").includes("/help"))).toBe(true);

    await rerender({ convo: "c1", live: null });
    expect(
      buttons(el).some((b) => (b.textContent ?? "").includes("/help")),
      "parked composer has no commands",
    ).toBe(false);
  });

  it("a parked slot's handlers are no-ops: the previous live onSend/onStop are never called", async () => {
    const live = makeLive({ isRunning: true });
    const el = await mount({ convo: "c1", live });
    await rerender({ convo: "c1", live: null });
    await act(async () => {
      textarea(el).dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
      );
      for (const b of buttons(el)) b.click();
    });
    expect(live.onSend).not.toHaveBeenCalled();
    expect(live.onStop).not.toHaveBeenCalled();
    expect(live.onPause).not.toHaveBeenCalled();
  });

  it("a live slot forwards its handlers: Enter sends text + attachment ids through live.onSend", async () => {
    const live = makeLive();
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "c1", live, handle });
    await add(handle, [pdf("spec.pdf")]);
    uploads[0].resolve(attachment("spec.pdf"));
    await flush();
    await typeInto(textarea(el), "see attached");
    await act(async () => {
      textarea(el).dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
      );
    });
    expect(live.onSend).toHaveBeenCalledTimes(1);
    expect(live.onSend).toHaveBeenCalledWith("see attached", [`att-${attSeq}`]);
  });
});

// ─── the imperative handle ───────────────────────────────────────────────

describe("ComposerSlot — ref.add", () => {
  it("exposes add(files) that uploads under the `convo` prop", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const f = pdf("a.pdf");
    await mount({ convo: "convo-1", live: makeLive(), handle });
    await add(handle, [f]);
    expect(uploadFileMock).toHaveBeenCalledTimes(1);
    expect(uploadFileMock).toHaveBeenCalledWith("convo-1", f, expect.any(Function));
  });

  it("uploads each file separately, all under the same convo", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const f1 = pdf("1.pdf");
    const f2 = pdf("2.pdf");
    await mount({ convo: "convo-1", live: makeLive(), handle });
    await add(handle, [f1, f2]);
    expect(uploads.map((u) => [u.convo, u.file])).toEqual([
      ["convo-1", f1],
      ["convo-1", f2],
    ]);
  });

  it("renders a chip for the added file", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [pdf("report.pdf")]);
    expect(removeChip(el, "report.pdf")).not.toBeNull();
  });

  it("uses the UPDATED convo after a convo change (not a stale closure)", async () => {
    const handle = createRef<ComposerSlotHandle>();
    await mount({ convo: "x", live: makeLive(), handle });
    await rerender({ convo: "y", live: makeLive(), handle });
    await add(handle, [pdf("late.pdf")]);
    expect(uploadFileMock).toHaveBeenCalledWith("y", expect.any(File), expect.any(Function));
  });

  it("honours live.uploadLimits: a file over max_file_mb becomes an error chip and is never uploaded", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({
      convo: "c1",
      live: makeLive({ uploadLimits: { max_file_mb: 1, max_files: 10 } }),
      handle,
    });
    const big = pdf("big.pdf");
    Object.defineProperty(big, "size", { value: 2 * 1024 * 1024 });
    await add(handle, [big]);
    expect(uploadFileMock).not.toHaveBeenCalled();
    expect(el.textContent).toContain("Too large");
  });

  it("with convo=null the add is rejected client-side ('Not connected') and nothing uploads", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: null, live: makeLive(), handle });
    await add(handle, [pdf("x.pdf")]);
    expect(uploadFileMock).not.toHaveBeenCalled();
    expect(el.textContent).toContain("Not connected");
  });

  it("the file input (📎) feeds the same slot attachments, under the same convo", async () => {
    const el = await mount({ convo: "c-input", live: makeLive() });
    const input = el.querySelector('input[type="file"]');
    expect(input, "slot must render the attach input").not.toBeNull();
    const f = pdf("picked.pdf");
    Object.defineProperty(input, "files", { value: [f], configurable: true });
    await act(async () => {
      input!.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(uploadFileMock).toHaveBeenCalledWith("c-input", f, expect.any(Function));
  });
});

// ─── keep-alive: state survives live → null → live ───────────────────────

describe("ComposerSlot — keep-alive across live → null → live", () => {
  it("keeps BOTH the typed text and the chips", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "c1", live: makeLive(), handle });
    await typeInto(textarea(el), "my draft");
    await add(handle, [pdf("keep.pdf")]);

    await rerender({ convo: "c1", live: null, handle });
    expect(textarea(el).value).toBe("my draft");
    expect(removeChip(el, "keep.pdf"), "chip survives while hidden").not.toBeNull();

    await rerender({ convo: "c1", live: makeLive(), handle });
    expect(textarea(el).value).toBe("my draft");
    expect(removeChip(el, "keep.pdf"), "chip survives re-activation").not.toBeNull();
    expect(isHidden(el, textarea(el))).toBe(false);
  });

  it("does not abort, revoke or re-upload on the round trip", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [png("pic.png")]);
    expect(uploads).toHaveLength(1);
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);

    await rerender({ convo: "c1", live: null, handle });
    await rerender({ convo: "c1", live: makeLive(), handle });

    expect(uploads, "no second upload").toHaveLength(1);
    expect(uploads[0].abort, "no abort").not.toHaveBeenCalled();
    expect(URL.revokeObjectURL, "preview must not be revoked").not.toHaveBeenCalled();
    expect(removeChip(el, "pic.png")).not.toBeNull();
  });

  it("survives repeated hide/show cycles", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "c1", live: makeLive(), handle });
    await typeInto(textarea(el), "cycle");
    await add(handle, [pdf("c.pdf")]);
    for (let i = 0; i < 5; i++) {
      await rerender({ convo: "c1", live: null, handle });
      await rerender({ convo: "c1", live: makeLive(), handle });
    }
    expect(textarea(el).value).toBe("cycle");
    expect(removeChip(el, "c.pdf")).not.toBeNull();
    expect(uploads).toHaveLength(1);
  });

  it("an inactive-from-the-start slot can be typed into later once live, and starts empty", async () => {
    const el = await mount({ convo: "c1", live: null });
    expect(textarea(el).value).toBe("");
    await rerender({ convo: "c1", live: makeLive() });
    expect(textarea(el).value).toBe("");
    await typeInto(textarea(el), "now typing");
    expect(textarea(el).value).toBe("now typing");
  });
});

// ─── deferred uploads resolving while hidden ─────────────────────────────

describe("ComposerSlot — uploads that finish while hidden", () => {
  it("a deferred upload resolved while hidden shows as a DONE chip when shown again", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [pdf("slow.pdf")]);
    // Still uploading: Send is blocked on "Waiting for uploads".
    expect(sendButton(el)!.disabled).toBe(true);
    expect(sendButton(el)!.title).toContain("Waiting for uploads");

    await rerender({ convo: "c1", live: null, handle });
    await act(async () => {
      uploads[0].resolve(attachment("slow.pdf"));
    });
    await flush();

    await rerender({ convo: "c1", live: makeLive(), handle });
    expect(removeChip(el, "slow.pdf")).not.toBeNull();
    const send = sendButton(el)!;
    expect(send.disabled, "done chip + ready => Send enabled with attachment only").toBe(false);
    expect(send.title).toBe("");
  });

  it("a done-while-hidden attachment is sent by id after re-activation", async () => {
    const live = makeLive();
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "c1", live, handle });
    await add(handle, [pdf("slow.pdf")]);
    await rerender({ convo: "c1", live: null, handle });
    const att = attachment("slow.pdf");
    await act(async () => {
      uploads[0].resolve(att);
    });
    await flush();
    const live2 = makeLive();
    await rerender({ convo: "c1", live: live2, handle });

    await act(async () => {
      sendButton(el)!.click();
    });
    expect(live2.onSend).toHaveBeenCalledWith("", [att.id]);
    expect(live.onSend).not.toHaveBeenCalled();
  });

  it("an upload that FAILS while hidden shows an error chip when shown again", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [pdf("bad.pdf")]);
    await rerender({ convo: "c1", live: null, handle });
    await act(async () => {
      uploads[0].reject(new Error("Upload failed (HTTP 500)"));
    });
    await flush();
    await rerender({ convo: "c1", live: makeLive(), handle });
    expect(el.textContent).toContain("Upload failed (HTTP 500)");
    expect(sendButton(el)!.disabled).toBe(true);
  });

  it("two uploads resolving out of order while hidden both end up done", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [pdf("one.pdf"), pdf("two.pdf")]);
    await rerender({ convo: "c1", live: null, handle });
    await act(async () => {
      uploads[1].resolve(attachment("two.pdf"));
      uploads[0].resolve(attachment("one.pdf"));
    });
    await flush();
    await rerender({ convo: "c1", live: makeLive(), handle });
    expect(removeChip(el, "one.pdf")).not.toBeNull();
    expect(removeChip(el, "two.pdf")).not.toBeNull();
    expect(sendButton(el)!.disabled).toBe(false);
  });
});

// ─── unmount cleanup ─────────────────────────────────────────────────────

describe("ComposerSlot — unmount (tab closed)", () => {
  it("aborts an in-flight upload and revokes its preview URL", async () => {
    const handle = createRef<ComposerSlotHandle>();
    await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [png("pic.png")]);
    expect(uploads[0].abort).not.toHaveBeenCalled();
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();

    await unmount();

    expect(uploads[0].abort).toHaveBeenCalledTimes(1);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview-1");
  });

  it("aborts EVERY in-flight upload, not just the first", async () => {
    const handle = createRef<ComposerSlotHandle>();
    await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [pdf("1.pdf"), pdf("2.pdf"), pdf("3.pdf")]);
    await unmount();
    expect(uploads.map((u) => u.abort.mock.calls.length)).toEqual([1, 1, 1]);
  });

  it("unmounting an INACTIVE (hidden) slot still aborts its upload", async () => {
    const handle = createRef<ComposerSlotHandle>();
    await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [pdf("bg.pdf")]);
    await rerender({ convo: "c1", live: null, handle });
    expect(uploads[0].abort).not.toHaveBeenCalled();
    await unmount();
    expect(uploads[0].abort).toHaveBeenCalledTimes(1);
  });

  it("does not abort an already-finished upload", async () => {
    const handle = createRef<ComposerSlotHandle>();
    await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [pdf("done.pdf")]);
    await act(async () => {
      uploads[0].resolve(attachment("done.pdf"));
    });
    await flush();
    await unmount();
    expect(uploads[0].abort).not.toHaveBeenCalled();
  });

  it("revokes previews of finished chips too (no leaked blob URLs)", async () => {
    const handle = createRef<ComposerSlotHandle>();
    await mount({ convo: "c1", live: makeLive(), handle });
    await add(handle, [png("pic.png")]);
    await act(async () => {
      uploads[0].resolve(attachment("pic.png"));
    });
    await flush();
    await unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview-1");
  });
});

// ─── convo prop changes ──────────────────────────────────────────────────

describe("ComposerSlot — `convo` prop changes", () => {
  it("null → 'u' resets the chips but keeps the typed text", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: null, live: makeLive(), handle });
    await typeInto(textarea(el), "typed in a new tab");
    await add(handle, [pdf("early.pdf")]); // convo null → error chip, not uploaded
    expect(removeChip(el, "early.pdf")).not.toBeNull();

    await rerender({ convo: "u", live: makeLive(), handle });

    expect(textarea(el).value).toBe("typed in a new tab");
    expect(removeChip(el, "early.pdf"), "chips are per-conversation: reset on bind").toBeNull();
    expect(el.textContent).not.toContain("Not connected");
  });

  it("'x' → 'y' aborts the in-flight upload, revokes its preview, clears chips, keeps text", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "x", live: makeLive(), handle });
    await typeInto(textarea(el), "keep me");
    await add(handle, [png("pic.png")]);
    expect(uploads[0].convo).toBe("x");

    await rerender({ convo: "y", live: makeLive(), handle });

    expect(textarea(el).value).toBe("keep me");
    expect(removeChip(el, "pic.png")).toBeNull();
    expect(uploads[0].abort).toHaveBeenCalledTimes(1);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview-1");
  });

  it("an upload that finishes AFTER the convo changed does not resurrect its chip", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "x", live: makeLive(), handle });
    await add(handle, [pdf("ghost.pdf")]);
    await rerender({ convo: "y", live: makeLive(), handle });
    await act(async () => {
      uploads[0].resolve(attachment("ghost.pdf", "x"));
    });
    await flush();
    expect(removeChip(el, "ghost.pdf")).toBeNull();
    expect(sendButton(el)!.disabled).toBe(true); // nothing to send
  });

  it("the convo change resets chips even while the slot is parked (inactive)", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "x", live: makeLive(), handle });
    await add(handle, [pdf("p.pdf")]);
    await rerender({ convo: "x", live: null, handle });
    await rerender({ convo: "y", live: null, handle });
    expect(removeChip(el, "p.pdf")).toBeNull();
    expect(uploads[0].abort).toHaveBeenCalledTimes(1);
  });

  it("re-rendering with the SAME convo keeps chips and does not abort", async () => {
    const handle = createRef<ComposerSlotHandle>();
    const el = await mount({ convo: "x", live: makeLive(), handle });
    await add(handle, [pdf("stay.pdf")]);
    await rerender({ convo: "x", live: makeLive(), handle });
    await rerender({ convo: "x", live: makeLive({ pendingInput: 3 }), handle });
    expect(removeChip(el, "stay.pdf")).not.toBeNull();
    expect(uploads[0].abort).not.toHaveBeenCalled();
  });

  it("new uploads after the change go to the new convo", async () => {
    const handle = createRef<ComposerSlotHandle>();
    await mount({ convo: "x", live: makeLive(), handle });
    await add(handle, [pdf("first.pdf")]);
    await rerender({ convo: "y", live: makeLive(), handle });
    await add(handle, [pdf("second.pdf")]);
    expect(uploads.map((u) => u.convo)).toEqual(["x", "y"]);
  });
});
