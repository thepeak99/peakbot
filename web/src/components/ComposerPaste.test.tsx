// Tests for pasting files/images (Ctrl+V / Cmd+V) into the composer textarea.
//
// Contract (feat/paste-images): a paste event whose clipboard carries files
// becomes pending attachments through the SAME `attachments.add(files)` path
// the 📎 button and DropOverlay use. Files can arrive two ways:
//
//   (a) `clipboardData.files`                      — Chrome/Firefox screenshots
//   (b) `clipboardData.items[]` with kind "file"   — files list empty, bytes only
//                                                    reachable via `getAsFile()`
//                                                    (some browsers / sources)
//
// Rules:
//   - files present      → preventDefault (no filename/garbage text in the box)
//                          and add() once with exactly those File objects.
//   - plain text only    → NOT intercepted: no preventDefault, no add().
//   - files AND items both list the same file (Chrome does this) → added ONCE.
//   - non-image files    → same path as attached files (no image-only filter).
//   - limits (max_files / max_file_mb) are enforced by the existing add() logic.
//   - empty paste on the Linux desktop app (no files, no items, no text)
//     → readClipboardImage() fallback: a File → add([file]) through the
//       SAME path; null (0 bytes) or a rejection (older app) → nothing
//       (WebKit Bug 218519: WebKitGTK ≤ 2.50 hands paste events no image).
//
// Two layers:
//   1. <Composer> with a fake `attachments` api  → "calls add with the Files".
//   2. <ComposerSlot> (real useAttachments, `../uploads` mocked) → "chips appear
//      and uploads start", exactly as if attached via the button.
//
// Status when written: layer (a) already works in the code base (Composer.onPaste
// reads `clipboardData.files`), so those tests are regression guards. Layer (b)
// (items fallback) is NOT implemented → those tests are the RED ones.
//
// Harness: createRoot + act + matchMedia stub, as in Composer.test.tsx /
// ComposerSlot.test.tsx. jsdom is automatic for src/components/**.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Composer } from "./Composer";
import { ComposerSlot, type ComposerLive } from "./ComposerSlot";
import { uploadFile } from "../uploads";
import type { WireAttachment } from "../state";

vi.mock("../uploads", () => ({ uploadFile: vi.fn() }));
const uploadFileMock = vi.mocked(uploadFile);

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── environment ─────────────────────────────────────────────────────────

let originalMatchMedia: typeof window.matchMedia | undefined;
let container: HTMLDivElement | null = null;
let root: Root | null = null;
let uploads: { file: File }[] = [];
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
  // Never-settling upload: chips stay "uploading"; enough to prove add() ran.
  uploadFileMock.mockImplementation((_convo, file) => {
    uploads.push({ file });
    return {
      done: new Promise<WireAttachment>(() => {}),
      abort: vi.fn(),
    };
  });
  URL.createObjectURL = vi.fn(() => `blob:preview-${(previewSeq += 1)}`);
  URL.revokeObjectURL = vi.fn();
});

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

const png = (name = "image.png"): File =>
  new File(["png-bytes"], name, { type: "image/png" });
const pdf = (name = "spec.pdf"): File =>
  new File(["pdf-bytes"], name, { type: "application/pdf" });

type ClipItem = { kind: string; type: string; getAsFile: () => File | null };
const fileItem = (f: File | null, type = f?.type ?? "image/png"): ClipItem => ({
  kind: "file",
  type,
  getAsFile: () => f,
});
const textItem = (): ClipItem => ({
  kind: "string",
  type: "text/plain",
  getAsFile: () => null,
});

/**
 * Dispatch a bubbling, cancelable `paste` on `target` with a fake
 * DataTransfer (jsdom has no ClipboardEvent/DataTransfer). Returns the event so
 * the test can read `defaultPrevented`.
 */
async function paste(
  target: Element,
  clip: { files?: File[]; items?: ClipItem[]; text?: string },
): Promise<Event> {
  const ev = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(ev, "clipboardData", {
    value: {
      files: clip.files ?? [],
      items: clip.items ?? [],
      types: [
        ...((clip.files?.length ?? 0) > 0 ? ["Files"] : []),
        ...(clip.text !== undefined ? ["text/plain"] : []),
      ],
      getData: (t: string) => (t === "text/plain" ? (clip.text ?? "") : ""),
    },
  });
  await act(async () => {
    target.dispatchEvent(ev);
  });
  return ev;
}

async function render(node: React.ReactElement): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(node);
  });
  return container;
}

const textareaOf = (el: HTMLElement): HTMLTextAreaElement => {
  const ta = el.querySelector("textarea");
  expect(ta, "expected the composer to render a <textarea>").not.toBeNull();
  return ta as HTMLTextAreaElement;
};

// ─── layer 1: <Composer> with a fake attachments api ─────────────────────

type AttachmentsApi = React.ComponentProps<typeof Composer>["attachments"];

function fakeApi(add: (files: File[]) => void): AttachmentsApi {
  return { pending: [], add, remove: vi.fn(), reset: vi.fn(), ids: [], ready: true };
}

function composerProps(add: (files: File[]) => void): React.ComponentProps<typeof Composer> {
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
    attachments: fakeApi(add),
  } as React.ComponentProps<typeof Composer>;
}

async function mountComposer(add: (files: File[]) => void): Promise<HTMLTextAreaElement> {
  const el = await render(<Composer {...composerProps(add)} />);
  return textareaOf(el);
}

describe("Composer paste — files on clipboardData.files", () => {
  it("passes a pasted image File to attachments.add", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const shot = png("image.png");

    await paste(ta, { files: [shot] });

    expect(add).toHaveBeenCalledTimes(1);
    expect(add).toHaveBeenCalledWith([shot]);
  });

  it("passes several pasted images to ONE add call, in clipboard order", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const a = png("image.png");
    const b = png("image.png");
    const c = png("shot.png");

    await paste(ta, { files: [a, b, c] });

    expect(add).toHaveBeenCalledTimes(1);
    const passed = add.mock.calls[0][0] as File[];
    expect(passed).toHaveLength(3);
    expect(passed[0]).toBe(a);
    expect(passed[1]).toBe(b);
    expect(passed[2]).toBe(c);
  });

  it("prevents the default paste when files are pasted (no filename/garbage text inserted)", async () => {
    const ta = await mountComposer(vi.fn());

    const ev = await paste(ta, { files: [png()] });

    expect(ev.defaultPrevented).toBe(true);
    expect(ta.value).toBe("");
  });

  it("prevents the default paste even when text/plain rides along with the image", async () => {
    // Word/Excel/browsers put a text rendition next to the image blob.
    const add = vi.fn();
    const ta = await mountComposer(add);

    const ev = await paste(ta, { files: [png()], text: "Screenshot 2026-11-02 at 12.00.00" });

    expect(ev.defaultPrevented).toBe(true);
    expect(add).toHaveBeenCalledTimes(1);
  });

  it("sends non-image clipboard files down the same add path as images", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const doc = pdf();
    const shot = png();

    const ev = await paste(ta, { files: [doc, shot] });

    expect(add).toHaveBeenCalledTimes(1);
    expect(add).toHaveBeenCalledWith([doc, shot]);
    expect(ev.defaultPrevented).toBe(true);
  });

  it("pasting only a non-image file is still attached (no image-only filter)", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const doc = pdf();

    await paste(ta, { files: [doc] });

    expect(add).toHaveBeenCalledWith([doc]);
  });
});

describe("Composer paste — plain text is NOT intercepted", () => {
  it("does not preventDefault and does not call add for a text-only paste", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);

    const ev = await paste(ta, { text: "just some text", items: [textItem()] });

    expect(ev.defaultPrevented, "text paste must keep the browser default").toBe(false);
    expect(add).not.toHaveBeenCalled();
  });

  it("ignores an empty clipboard (no files, no items)", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);

    const ev = await paste(ta, {});

    expect(ev.defaultPrevented).toBe(false);
    expect(add).not.toHaveBeenCalled();
  });

  it("does not intercept when a file item yields no File (getAsFile() === null)", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);

    const ev = await paste(ta, { text: "x", items: [fileItem(null, "image/png"), textItem()] });

    expect(ev.defaultPrevented).toBe(false);
    expect(add).not.toHaveBeenCalled();
  });
});

describe("Composer paste — files only reachable via clipboardData.items", () => {
  // RED today: Composer.onPaste only reads `clipboardData.files`.
  it("falls back to items[kind=file].getAsFile() when clipboardData.files is empty", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const shot = png("image.png");

    const ev = await paste(ta, { files: [], items: [fileItem(shot)] });

    expect(add).toHaveBeenCalledTimes(1);
    expect(add).toHaveBeenCalledWith([shot]);
    expect(ev.defaultPrevented).toBe(true);
  });

  it("collects every file item (several images) in item order via the fallback", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const a = png("image.png");
    const b = png("image.png");

    await paste(ta, { files: [], items: [fileItem(a), fileItem(b)] });

    expect(add).toHaveBeenCalledTimes(1);
    const passed = add.mock.calls[0][0] as File[];
    expect(passed).toHaveLength(2);
    expect(passed[0]).toBe(a);
    expect(passed[1]).toBe(b);
  });

  it("takes only the file items from a mixed string + file clipboard", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const shot = png();

    const ev = await paste(ta, {
      files: [],
      text: "caption",
      items: [textItem(), fileItem(shot)],
    });

    expect(add).toHaveBeenCalledWith([shot]);
    expect(ev.defaultPrevented).toBe(true);
  });

  it("skips file items whose getAsFile() is null but still adds the usable ones", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const shot = png();

    await paste(ta, { files: [], items: [fileItem(null, "image/png"), fileItem(shot)] });

    expect(add).toHaveBeenCalledWith([shot]);
  });

  it("fallback also accepts non-image file items (same path as attach)", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const doc = pdf();

    await paste(ta, { files: [], items: [fileItem(doc)] });

    expect(add).toHaveBeenCalledWith([doc]);
  });

  // Guard against a naive "files + items" union: Chrome lists the same
  // screenshot in BOTH, which would attach it twice.
  it("adds a file once when it appears in both files and items", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const shot = png();

    await paste(ta, { files: [shot], items: [fileItem(shot)] });

    expect(add).toHaveBeenCalledTimes(1);
    expect(add).toHaveBeenCalledWith([shot]);
  });
});

// ─── Linux desktop clipboard fallback (WebKit Bug 218519) ────────────────
// WebKitGTK ≤ 2.50 (the bundled desktop) never exposes a clipboard image
// to DOM paste events, so an empty paste (no files, no items, no text)
// on the Linux desktop app asks the app for the clipboard image via
// readClipboardImage() (`clipboard_image_png` IPC): a File → add([file])
// through the SAME path; null (0 bytes) or a rejection (older app) →
// nothing. Never preventDefault — the default for a paste with no files
// and no text inserts nothing.
//
// RED today: Composer.onPaste has no Linux fallback, so L1/L2/L3/L8 fail
// (the IPC is never called). L4–L7 are regression guards and are green
// before the implementation.
//
// The UA is ALWAYS stubbed: jsdom's default UA is "(linux)" on Linux CI
// and "(darwin)" on a Mac, so leaving it unstubbed is non-deterministic.

const WEBKITGTK_UA =
  "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)";

describe("Composer paste — Linux desktop clipboard fallback (WebKit Bug 218519)", () => {
  let invoke: ReturnType<typeof vi.fn>;
  let uaSpy: ReturnType<typeof vi.spyOn>;

  // Settle the fire-and-forget readClipboardImage().then(…) chain after
  // paste() so the attachment lands inside act.
  const flush = () => act(async () => {});

  beforeEach(() => {
    (globalThis as { isTauri?: unknown }).isTauri = true;
    invoke = vi.fn(async () => new Uint8Array([1, 2, 3]).buffer);
    (globalThis as { __TAURI__?: unknown }).__TAURI__ = { core: { invoke } };
    uaSpy = vi.spyOn(navigator, "userAgent", "get").mockReturnValue(WEBKITGTK_UA);
  });

  afterEach(() => {
    delete (globalThis as { isTauri?: unknown }).isTauri;
    delete (globalThis as { __TAURI__?: unknown }).__TAURI__;
    vi.restoreAllMocks();
  });

  it("an empty paste on the Linux desktop adds the clipboard image as pasted-image.png", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);

    const ev = await paste(ta, {});
    await flush();

    expect(add).toHaveBeenCalledTimes(1);
    const passed = add.mock.calls[0][0] as File[];
    expect(passed).toHaveLength(1);
    expect(passed[0].name).toBe("pasted-image.png");
    expect(passed[0].type).toBe("image/png");
    expect(passed[0].size).toBe(3);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("clipboard_image_png");
    expect(ev.defaultPrevented).toBe(false);
  });

  it("an empty clipboard image (0 bytes) adds nothing", async () => {
    invoke.mockImplementation(async () => new ArrayBuffer(0));
    const add = vi.fn();
    const ta = await mountComposer(add);

    await paste(ta, {});
    await flush();

    expect(add).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("a rejected command (older app) adds nothing and does not throw", async () => {
    // The rejection is swallowed by the fallback; Vitest's
    // unhandled-rejection check is the "does not throw" proof.
    invoke.mockImplementation(async () => {
      throw "command clipboard_image_png not found";
    });
    const add = vi.fn();
    const ta = await mountComposer(add);

    await paste(ta, {});
    await flush();

    expect(add).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("a paste with text keeps the default and never asks the app", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);

    const ev = await paste(ta, { text: "hi" });
    await flush();

    expect(invoke).not.toHaveBeenCalled();
    expect(ev.defaultPrevented).toBe(false);
  });

  it("a paste with files uses the files and never asks the app", async () => {
    const add = vi.fn();
    const ta = await mountComposer(add);
    const shot = png();

    await paste(ta, { files: [shot] });
    await flush();

    expect(add).toHaveBeenCalledWith([shot]);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("in a browser an empty paste never asks the app", async () => {
    delete (globalThis as { isTauri?: unknown }).isTauri;
    const add = vi.fn();
    const ta = await mountComposer(add);

    await paste(ta, {});
    await flush();

    expect(invoke).not.toHaveBeenCalled();
    expect(add).not.toHaveBeenCalled();
  });

  it.each([
    [
      "macOS",
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)",
    ],
    [
      "Windows",
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0",
    ],
  ])("on the %s desktop app an empty paste never asks the app", async (_os, ua) => {
    uaSpy.mockReturnValue(ua);
    const add = vi.fn();
    const ta = await mountComposer(add);

    await paste(ta, {});
    await flush();

    expect(invoke).not.toHaveBeenCalled();
  });

  it("a Linux-desktop clipboard image becomes a chip and starts an upload", async () => {
    const ta = await mountSlot();

    await paste(ta, {});
    await flush();

    expect(uploads).toHaveLength(1);
    expect(uploads[0].file.name).toBe("pasted-image.png");
  });
});

// ─── layer 2: <ComposerSlot> with the real useAttachments ────────────────

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

async function mountSlot(
  live: ComposerLive = makeLive(),
  convo: string | null = "c1",
): Promise<HTMLTextAreaElement> {
  const el = await render(<ComposerSlot convo={convo} live={live} />);
  return textareaOf(el);
}

const chipRemovers = (name: string): Element[] =>
  Array.from(document.querySelectorAll(`[aria-label="Remove ${name}"]`));

describe("Composer paste → pending attachments (real useAttachments)", () => {
  it("pasting an image renders a chip and starts an upload, like the attach button", async () => {
    const ta = await mountSlot();
    const shot = png("image.png");

    await paste(ta, { files: [shot] });

    expect(chipRemovers("image.png")).toHaveLength(1);
    expect(uploads).toHaveLength(1);
    expect(uploads[0].file).toBe(shot);
    expect(uploadFileMock).toHaveBeenCalledWith("c1", shot, expect.any(Function));
  });

  it("pasting an image that is only in clipboardData.items renders a chip and uploads", async () => {
    // RED today (items fallback missing).
    const ta = await mountSlot();
    const shot = png("image.png");

    const ev = await paste(ta, { files: [], items: [fileItem(shot)] });

    expect(ev.defaultPrevented).toBe(true);
    expect(chipRemovers("image.png")).toHaveLength(1);
    expect(uploads.map((u) => u.file)).toEqual([shot]);
  });

  it("pasting a non-image file renders a chip too (files of any type are allowed)", async () => {
    const ta = await mountSlot();

    await paste(ta, { files: [pdf("spec.pdf")] });

    expect(chipRemovers("spec.pdf")).toHaveLength(1);
    expect(uploads).toHaveLength(1);
  });

  it("two pasted images that both arrive named image.png become two separate attachments", async () => {
    // Collision analysis: chip keys come from a per-hook counter (`a1`, `a2`…),
    // never from file.name, and the server stores each upload under its own
    // Uuid dir — so identical names cannot overwrite. This is a regression
    // guard, expected green before AND after the feature.
    const ta = await mountSlot();
    const first = png("image.png");
    const second = png("image.png");

    await paste(ta, { files: [first, second] });

    expect(chipRemovers("image.png")).toHaveLength(2);
    expect(uploads.map((u) => u.file)).toEqual([first, second]);
    expect(uploads[0].file).not.toBe(uploads[1].file);
  });

  it("two SEPARATE pastes of image.png accumulate rather than replace each other", async () => {
    const ta = await mountSlot();

    await paste(ta, { files: [png("image.png")] });
    await paste(ta, { files: [png("image.png")] });

    expect(chipRemovers("image.png")).toHaveLength(2);
    expect(uploads).toHaveLength(2);
  });

  it("two identically named images via the items fallback also stay distinct", async () => {
    // RED today (items fallback missing).
    const ta = await mountSlot();
    const a = png("image.png");
    const b = png("image.png");

    await paste(ta, { files: [], items: [fileItem(a), fileItem(b)] });

    expect(chipRemovers("image.png")).toHaveLength(2);
    expect(uploads.map((u) => u.file)).toEqual([a, b]);
  });

  it("a text-only paste adds no chip, starts no upload, and keeps the default", async () => {
    const ta = await mountSlot();

    const ev = await paste(ta, { text: "hello", items: [textItem()] });

    expect(ev.defaultPrevented).toBe(false);
    expect(uploads).toHaveLength(0);
    expect(document.querySelectorAll('[aria-label^="Remove "]')).toHaveLength(0);
  });

  it("pasted files obey the max_files limit via the existing add logic", async () => {
    const ta = await mountSlot(makeLive({ uploadLimits: { max_file_mb: 50, max_files: 2 } }));

    await paste(ta, { files: [png("a.png"), png("b.png"), png("c.png")] });

    expect(uploads, "only the first two reach the server").toHaveLength(2);
    expect(container!.textContent).toContain("Too many files (max 2)");
  });

  it("a pasted file over max_file_mb is rejected client-side with the existing message", async () => {
    const ta = await mountSlot(makeLive({ uploadLimits: { max_file_mb: 1, max_files: 10 } }));
    const big = new File(["x"], "huge.png", { type: "image/png" });
    Object.defineProperty(big, "size", { value: 2 * 1024 * 1024 });

    await paste(ta, { files: [big] });

    expect(uploads).toHaveLength(0);
    expect(container!.textContent).toContain("Too large");
  });

  it("pasting before a conversation exists shows the existing 'Not connected' error chip", async () => {
    const ta = await mountSlot(makeLive(), null);

    await paste(ta, { files: [png()] });

    expect(uploads).toHaveLength(0);
    expect(container!.textContent).toContain("Not connected");
  });
});
