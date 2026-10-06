// @vitest-environment jsdom
//
// RED (T11): `useAttachments.ts` does not exist yet. Written against §5.7:
//
//   useAttachments(convo, limits) → { pending, add, remove, reset, ids, ready }
//
//   - pre-checks: size > max_file_mb → error chip, never uploaded;
//     count > max_files → the extra chip is an error chip
//   - ready = no "uploading" and no "error" chips
//   - remove() during an upload calls that chip's abort()
//   - a convo change resets: in-flight XHRs are aborted and chips cleared
//   - ids = the done attachments' ids, in chip order (not resolution order)
//
// `./uploads` is mocked so no XHR is ever opened; each test gets a control
// handle to resolve in-flight uploads out of order.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useAttachments } from "./useAttachments";
import { uploadFile } from "./uploads";
import type { WireAttachment } from "./state";

vi.mock("./uploads", () => ({ uploadFile: vi.fn() }));

const uploadFileMock = vi.mocked(uploadFile);

// §5.7: WireAppState.upload_limits — the hook's second argument.
type UploadLimits = { max_file_mb: number; max_files: number };

const CONVO = "11111111-1111-4111-8111-111111111111";
const OTHER_CONVO = "33333333-3333-4333-8333-333333333333";
const LIMITS: UploadLimits = { max_file_mb: 50, max_files: 10 };

let attSeq = 0;
function attachment(name: string, kind: "image" | "file" = "file"): WireAttachment {
  attSeq += 1;
  return {
    id: `id-${attSeq}`,
    convo: CONVO,
    name,
    mime: kind === "image" ? "image/png" : "application/pdf",
    size: 1234,
    kind,
  };
}

// ─── mocked uploadFile control ────────────────────────────────────────────

interface UploadControl {
  calls: { convo: string; file: File }[];
  /** The abort() fn handed to the hook for the Nth upload. */
  abort: (index: number) => ReturnType<typeof vi.fn>;
  /** Resolve the Nth in-flight upload with a stored attachment. */
  resolve: (index: number, att: WireAttachment) => void;
}

function mockUploads(): UploadControl {
  const calls: UploadControl["calls"] = [];
  const aborts = new Map<number, ReturnType<typeof vi.fn>>();
  const resolvers = new Map<number, (a: WireAttachment) => void>();
  uploadFileMock.mockImplementation((convo, file) => {
    const index = calls.length;
    calls.push({ convo, file });
    const abort = vi.fn();
    aborts.set(index, abort);
    return {
      done: new Promise<WireAttachment>((resolve) => {
        resolvers.set(index, resolve);
      }),
      abort,
    };
  });
  return {
    calls,
    abort: (i) => aborts.get(i)!,
    resolve: (i, att) => resolvers.get(i)?.(att),
  };
}

// ─── hook harness (createRoot + act, per the component-test convention) ──

let api: ReturnType<typeof useAttachments> | undefined;

function Harness({ convo, limits }: { convo: string | null; limits: UploadLimits }) {
  api = useAttachments(convo, limits);
  return null;
}

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function renderHook(convo: string | null, limits: UploadLimits): Promise<void> {
  await act(async () => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<Harness convo={convo} limits={limits} />);
  });
}

async function rerender(convo: string | null, limits: UploadLimits): Promise<void> {
  await act(async () => {
    root!.render(<Harness convo={convo} limits={limits} />);
  });
}

// Flush a resolved upload promise: microtask for the .then, macrotask for
// the React state update the hook schedules from it.
async function flushUpload(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  attSeq = 0;
  // jsdom has no object URLs; the hook may create previews for thumbnailable
  // types, so stub both.
  URL.createObjectURL = vi.fn(() => "blob:fake");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  vi.clearAllMocks();
  delete (URL as unknown as Record<string, unknown>)["createObjectURL"];
  delete (URL as unknown as Record<string, unknown>)["revokeObjectURL"];
});

// ─── tests ────────────────────────────────────────────────────────────────

describe("useAttachments", () => {
  it("starts one upload per file and is not ready while uploading", async () => {
    const ctrl = mockUploads();
    await renderHook(CONVO, LIMITS);
    const file = new File(["x"], "a.pdf", { type: "application/pdf" });

    await act(async () => {
      api!.add([file]);
    });

    expect(ctrl.calls).toEqual([{ convo: CONVO, file }]);
    expect(api!.pending).toHaveLength(1);
    expect(api!.pending[0].state).toBe("uploading");
    expect(api!.ready).toBe(false);
    expect(api!.ids).toEqual([]);
  });

  it("is ready with no chips", async () => {
    await renderHook(CONVO, LIMITS);
    expect(api!.pending).toEqual([]);
    expect(api!.ids).toEqual([]);
    expect(api!.ready).toBe(true);
  });

  it("flips a chip to done and ready=true once every upload resolves", async () => {
    const ctrl = mockUploads();
    await renderHook(CONVO, LIMITS);
    const att = attachment("a.pdf");

    await act(async () => {
      api!.add([new File(["x"], "a.pdf", { type: "application/pdf" })]);
    });
    expect(api!.ready).toBe(false);

    ctrl.resolve(0, att);
    await flushUpload();

    expect(api!.pending[0]).toMatchObject({ state: "done", attachment: att });
    expect(api!.ready).toBe(true);
    expect(api!.ids).toEqual([att.id]);
  });

  it("rejects an oversize file with an error chip and never uploads it", async () => {
    const ctrl = mockUploads();
    await renderHook(CONVO, { max_file_mb: 1, max_files: 10 });
    const big = new File(["x".repeat(2 * 1024 * 1024)], "big.bin", {
      type: "application/octet-stream",
    });

    await act(async () => {
      api!.add([big]);
    });

    expect(ctrl.calls, "oversize file must never reach uploadFile").toHaveLength(0);
    expect(api!.pending).toHaveLength(1);
    expect(api!.pending[0]).toMatchObject({ state: "error" });
    expect((api!.pending[0] as { message: string }).message).toMatch(/too large/i);
    expect(api!.ready).toBe(false);
  });

  it("marks the 11th file (max_files 10) as an error chip and uploads only the first 10", async () => {
    const ctrl = mockUploads();
    await renderHook(CONVO, LIMITS);
    const files = Array.from(
      { length: 11 },
      (_, i) => new File(["x"], `f${i}.pdf`, { type: "application/pdf" }),
    );

    await act(async () => {
      api!.add(files);
    });

    expect(ctrl.calls).toHaveLength(10);
    expect(api!.pending).toHaveLength(11);
    expect(
      api!.pending.slice(0, 10).every((c) => c.state === "uploading"),
    ).toBe(true);
    expect(api!.pending[10]).toMatchObject({ state: "error" });
    expect((api!.pending[10] as { message: string }).message).toMatch(
      /too many files/i,
    );
    expect(api!.ready).toBe(false);
  });

  it("remove() during an upload aborts the in-flight request and drops the chip", async () => {
    const ctrl = mockUploads();
    await renderHook(CONVO, LIMITS);

    await act(async () => {
      api!.add([new File(["x"], "a.pdf", { type: "application/pdf" })]);
    });
    const key = api!.pending[0].key;

    await act(async () => {
      api!.remove(key);
    });

    expect(ctrl.abort(0)).toHaveBeenCalled();
    expect(api!.pending).toEqual([]);
  });

  it("resets (aborts in-flight, clears chips) when the convo changes", async () => {
    const ctrl = mockUploads();
    await renderHook(CONVO, LIMITS);

    await act(async () => {
      api!.add([new File(["x"], "a.pdf", { type: "application/pdf" })]);
    });
    expect(api!.pending).toHaveLength(1);

    await rerender(OTHER_CONVO, LIMITS);

    expect(api!.pending, "chips must be cleared on convo change").toEqual([]);
    expect(ctrl.abort(0), "in-flight upload must be aborted").toHaveBeenCalled();
    expect(api!.ready).toBe(true);
  });

  it("preserves chip order in ids regardless of resolution order", async () => {
    const ctrl = mockUploads();
    await renderHook(CONVO, LIMITS);
    const files = [
      new File(["1"], "a.pdf", { type: "application/pdf" }),
      new File(["2"], "b.pdf", { type: "application/pdf" }),
      new File(["3"], "c.pdf", { type: "application/pdf" }),
    ];
    const atts = [attachment("a.pdf"), attachment("b.pdf"), attachment("c.pdf")];

    await act(async () => {
      api!.add(files);
    });

    // Resolve out of chip order: 3rd, then 1st, then 2nd.
    ctrl.resolve(2, atts[2]);
    await flushUpload();
    expect(api!.ids).toEqual([atts[2].id]);

    ctrl.resolve(0, atts[0]);
    await flushUpload();
    ctrl.resolve(1, atts[1]);
    await flushUpload();

    expect(api!.ids).toEqual([atts[0].id, atts[1].id, atts[2].id]);
  });
});
