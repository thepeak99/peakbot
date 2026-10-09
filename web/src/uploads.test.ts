// @vitest-environment jsdom
//
// RED (T10): `uploads.ts` does not exist yet. Written against §5.7:
//
//   uploadFile(convo, file, onProgress) → { done: Promise<WireAttachment>,
//     abort: () => void }
//
//   XHR POST /api/uploads?convo=<uuid>&name=<encodeURIComponent(file.name)>
//   with Content-Type: application/octet-stream and the File as the raw
//   body; `upload.onprogress` drives onProgress with a 0..1 fraction; the
//   promise resolves with the parsed Attachment JSON on 200 and rejects
//   with the server {error} text on non-2xx; abort() rejects.
//
// A fake XMLHttpRequest replaces the global so no network is touched; each
// test grabs the instance and drives progress/response/abort manually.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { uploadFile } from "./uploads";
import type { WireAttachment } from "./state";

class FakeXHR {
  static instances: FakeXHR[] = [];
  method = "";
  url = "";
  headers: Record<string, string> = {};
  body: unknown = null;
  status = 0;
  responseText = "";
  response: unknown = null;
  upload: {
    onprogress: ((e: { loaded: number; total: number }) => void) | null;
  } = { onprogress: null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;

  constructor() {
    FakeXHR.instances.push(this);
  }
  open(method: string, url: string): void {
    this.method = method;
    this.url = url;
  }
  setRequestHeader(name: string, value: string): void {
    this.headers[name] = value;
  }
  send(body: unknown): void {
    this.body = body;
  }
  abort(): void {
    this.onabort?.();
  }

  // ── test drivers ────────────────────────────────────────────────────────
  emitProgress(loaded: number, total: number): void {
    this.upload.onprogress?.({ loaded, total });
  }
  respond(status: number, body: string): void {
    this.status = status;
    this.responseText = body;
    this.response = body;
    this.onload?.();
  }
}

const CONVO = "11111111-1111-4111-8111-111111111111";

const ATTACHMENT: WireAttachment = {
  id: "22222222-2222-4222-8222-222222222222",
  convo: CONVO,
  name: "spec.pdf",
  mime: "application/pdf",
  size: 2411724,
  kind: "file",
};

beforeEach(() => {
  FakeXHR.instances = [];
  vi.stubGlobal("XMLHttpRequest", FakeXHR);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("uploadFile", () => {
  it("POSTs the raw file to /api/uploads?convo=…&name=<urlencoded> as application/octet-stream", () => {
    const file = new File(["pdf-bytes"], "my spec.pdf", { type: "application/pdf" });
    uploadFile(CONVO, file, () => {});

    const xhr = FakeXHR.instances.at(-1)!;
    expect(xhr.method).toBe("POST");
    expect(xhr.url).toBe(
      `/api/uploads?convo=${CONVO}&name=${encodeURIComponent("my spec.pdf")}`,
    );
    expect(xhr.headers["Content-Type"]).toBe("application/octet-stream");
    expect(xhr.body).toBe(file);
  });

  it("reports upload progress as a 0..1 fraction", () => {
    const file = new File(["x".repeat(100)], "x.bin", {
      type: "application/octet-stream",
    });
    const fractions: number[] = [];
    uploadFile(CONVO, file, (f) => fractions.push(f));

    const xhr = FakeXHR.instances.at(-1)!;
    xhr.emitProgress(25, 100);
    xhr.emitProgress(100, 100);
    expect(fractions).toEqual([0.25, 1]);
  });

  it("resolves with the parsed Attachment on 200", async () => {
    const file = new File(["pdf-bytes"], "spec.pdf", { type: "application/pdf" });
    const { done } = uploadFile(CONVO, file, () => {});

    FakeXHR.instances.at(-1)!.respond(200, JSON.stringify(ATTACHMENT));
    await expect(done).resolves.toEqual(ATTACHMENT);
  });

  it("rejects with the server {error} text on 413", async () => {
    const file = new File(["x".repeat(1024)], "spec.pdf", { type: "application/pdf" });
    const { done } = uploadFile(CONVO, file, () => {});

    FakeXHR.instances.at(-1)!.respond(
      413,
      JSON.stringify({ error: "spec.pdf is larger than 50 MB" }),
    );
    await expect(done).rejects.toThrow("spec.pdf is larger than 50 MB");
  });

  it("rejects when abort() is called", async () => {
    const file = new File(["x"], "x.bin", { type: "application/octet-stream" });
    const { done, abort } = uploadFile(CONVO, file, () => {});

    abort();
    await expect(done).rejects.toBeInstanceOf(Error);
  });
});
