// @vitest-environment jsdom
//
// RED (remote-profiles T9): `uploadFile` has no base argument yet. Written
// against the design contract (remote-profiles-design §5.4, §6.2, §6.4):
// in a REMOTE conversation the upload POSTs to
// `/r/<L>/api/uploads?convo=<uuid>&name=<s>` (the local proxy, local
// cookie); in a local conversation the existing
// `/api/uploads?convo=<uuid>&name=<s>` stands. `<L>` arrives pre-encoded
// from `apiBase(profile, catalog)` and is used verbatim.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { uploadFile } from "./uploads";

class FakeXHR {
  static instances: FakeXHR[] = [];
  method = "";
  url = "";
  headers: Record<string, string> = {};
  body: unknown = null;
  status = 0;
  responseText = "";
  upload: { onprogress: ((e: { loaded: number; total: number }) => void) | null } = {
    onprogress: null,
  };
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
}

const CONVO = "11111111-1111-4111-8111-111111111111";

beforeEach(() => {
  FakeXHR.instances = [];
  vi.stubGlobal("XMLHttpRequest", FakeXHR);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("uploadFile — the /r/<L>/ base (remote-profiles T9)", () => {
  it("posts to /r/<L>/api/uploads?convo=…&name=… for a remote conversation", () => {
    const file = new File(["bytes"], "a.png", { type: "image/png" });
    uploadFile(CONVO, file, () => {}, "/r/home-dev");

    const xhr = FakeXHR.instances.at(-1)!;
    expect(xhr.method).toBe("POST");
    expect(xhr.url).toBe(
      `/r/home-dev/api/uploads?convo=${CONVO}&name=${encodeURIComponent("a.png")}`,
    );
    expect(xhr.headers["Content-Type"]).toBe("application/octet-stream");
    expect(xhr.body).toBe(file);
  });

  it("keeps the local /api/uploads URL with no base (unchanged)", () => {
    const file = new File(["bytes"], "a.png", { type: "image/png" });
    uploadFile(CONVO, file, () => {}, "");

    expect(FakeXHR.instances.at(-1)!.url).toBe(
      `/api/uploads?convo=${CONVO}&name=${encodeURIComponent("a.png")}`,
    );
  });

  it("uses the base verbatim when omitted (legacy 3-arg call)", () => {
    const file = new File(["bytes"], "a.png", { type: "image/png" });
    uploadFile(CONVO, file, () => {});

    expect(FakeXHR.instances.at(-1)!.url).toBe(
      `/api/uploads?convo=${CONVO}&name=${encodeURIComponent("a.png")}`,
    );
  });

  it("uses a URL-encoded base verbatim (apiBase pre-encodes the name)", () => {
    const file = new File(["bytes"], "a.png", { type: "image/png" });
    uploadFile(CONVO, file, () => {}, "/r/home%20dev");

    expect(FakeXHR.instances.at(-1)!.url).toBe(
      `/r/home%20dev/api/uploads?convo=${CONVO}&name=${encodeURIComponent("a.png")}`,
    );
  });
});
