// Tests for DirectoryBrowser (design §7.3).
//
// DirectoryBrowser is the extracted browse/commit body, hosted by both
// CwdPicker (desktop popover) and the phone Directory tab (PanelsSheet).
// Tests the mount effect (list_dir + request_recent_dirs), commit semantics
// (same path → no-op, transcript confirm guard), and phone vs desktop classes.
//
// RED reason: "Cannot find module './DirectoryBrowser'" — the feature is absent.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { DirectoryBrowser } from "./DirectoryBrowser";
import type { DirListing, InboundMessage } from "../state";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

interface DBProps {
  cwd: string;
  hasTranscript: boolean;
  dirListing: DirListing | null;
  recentDirs: string[];
  send: (msg: InboundMessage) => void;
  onSwitchCwd: (path: string) => void;
  onDone: () => void;
  phone?: boolean;
}

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mountDB(props: DBProps): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(
      <DirectoryBrowser
        cwd={props.cwd}
        hasTranscript={props.hasTranscript}
        dirListing={props.dirListing}
        recentDirs={props.recentDirs}
        send={props.send}
        onSwitchCwd={props.onSwitchCwd}
        onDone={props.onDone}
        phone={props.phone}
      />,
    );
  });
  return container;
}

afterEach(() => {
  vi.restoreAllMocks();
  act(() => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
});

describe("DirectoryBrowser — mount effect", () => {
  it("mount sends list_dir then request_recent_dirs", async () => {
    const sent: InboundMessage[] = [];
    await mountDB({
      cwd: "/project",
      hasTranscript: false,
      dirListing: null,
      recentDirs: [],
      send: (m) => sent.push(m),
      onSwitchCwd: () => {},
      onDone: () => {},
    });
    expect(sent).toContainEqual({ type: "list_dir", path: "/project" });
    expect(sent).toContainEqual({ type: "request_recent_dirs" });
  });
});

describe("DirectoryBrowser — commit semantics", () => {
  it("same path → onDone called, onSwitchCwd not called", async () => {
    const onSwitchCwd = vi.fn();
    const onDone = vi.fn();
    const el = await mountDB({
      cwd: "/project",
      hasTranscript: true,
      dirListing: { path: "/project", parent: "/", entries: [], error: null },
      recentDirs: [],
      send: () => {},
      onSwitchCwd,
      onDone,
    });
    const useBtn = Array.from(el.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").trim() === "Use this directory",
    );
    expect(useBtn, "'Use this directory' button renders").not.toBeUndefined();
    await act(async () => {
      useBtn!.click();
    });
    expect(onDone).toHaveBeenCalledTimes(1);
    expect(onSwitchCwd).not.toHaveBeenCalled();
  });

  it("different path with transcript + confirm false → onDone, no switch", async () => {
    const onSwitchCwd = vi.fn();
    const onDone = vi.fn();
    vi.spyOn(window, "confirm").mockReturnValue(false);
    const el = await mountDB({
      cwd: "/project",
      hasTranscript: true,
      dirListing: { path: "/other", parent: "/", entries: [], error: null },
      recentDirs: [],
      send: () => {},
      onSwitchCwd,
      onDone,
    });
    const useBtn = Array.from(el.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").trim() === "Use this directory",
    );
    await act(async () => {
      useBtn!.click();
    });
    expect(onDone).toHaveBeenCalledTimes(1);
    expect(onSwitchCwd).not.toHaveBeenCalled();
  });

  it("different path with transcript + confirm true → switch(path)", async () => {
    const onSwitchCwd = vi.fn();
    const onDone = vi.fn();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const el = await mountDB({
      cwd: "/project",
      hasTranscript: true,
      dirListing: { path: "/other", parent: "/", entries: [], error: null },
      recentDirs: [],
      send: () => {},
      onSwitchCwd,
      onDone,
    });
    const useBtn = Array.from(el.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").trim() === "Use this directory",
    );
    await act(async () => {
      useBtn!.click();
    });
    expect(onSwitchCwd).toHaveBeenCalledWith("/other");
  });

  it("clicking a recent row commits it", async () => {
    const onSwitchCwd = vi.fn();
    const onDone = vi.fn();
    const el = await mountDB({
      cwd: "/project",
      hasTranscript: false,
      dirListing: null,
      recentDirs: ["/recent/path"],
      send: () => {},
      onSwitchCwd,
      onDone,
    });
    const row = Array.from(el.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").includes("/recent/path"),
    );
    expect(row, "recent row renders").not.toBeUndefined();
    await act(async () => {
      row!.click();
    });
    expect(onSwitchCwd).toHaveBeenCalledWith("/recent/path");
  });

  it("clicking a dir row sends list_dir", async () => {
    const sent: InboundMessage[] = [];
    const el = await mountDB({
      cwd: "/project",
      hasTranscript: false,
      dirListing: {
        path: "/project",
        parent: "/",
        entries: [{ name: "sub", is_dir: true, full_path: "/project/sub" }],
        error: null,
      },
      recentDirs: [],
      send: (m) => sent.push(m),
      onSwitchCwd: () => {},
      onDone: () => {},
    });
    const row = Array.from(el.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").includes("sub"),
    );
    await act(async () => {
      row!.click();
    });
    expect(sent).toContainEqual({ type: "list_dir", path: "/project/sub" });
  });
});

describe("DirectoryBrowser — error listing", () => {
  it("renders error inline", async () => {
    const el = await mountDB({
      cwd: "/project",
      hasTranscript: false,
      dirListing: { path: "/project", parent: "/", entries: [], error: "permission denied" },
      recentDirs: [],
      send: () => {},
      onSwitchCwd: () => {},
      onDone: () => {},
    });
    expect(el.textContent).toContain("permission denied");
  });
});

describe("DirectoryBrowser — phone vs desktop classes", () => {
  it("phone: commit bar has sticky + bottom-0, body has no overflow-y-auto", async () => {
    const el = await mountDB({
      cwd: "/project",
      hasTranscript: false,
      dirListing: null,
      recentDirs: [],
      send: () => {},
      onSwitchCwd: () => {},
      onDone: () => {},
      phone: true,
    });
    const useBtn = Array.from(el.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").trim() === "Use this directory",
    );
    expect(useBtn, "'Use this directory' button renders").toBeDefined();
    const commitBar = useBtn!.closest(".sticky");
    expect(commitBar, "phone commit bar is sticky").not.toBeNull();
    expect(commitBar!.className).toContain("bottom-0");
    // Body should not have overflow-y-auto (the Sheet scrolls)
    const body = el.querySelector(".overflow-y-auto");
    expect(body, "phone body has no overflow-y-auto").toBeNull();
  });

  it("not phone: body has overflow-y-auto (desktop parity)", async () => {
    const el = await mountDB({
      cwd: "/project",
      hasTranscript: false,
      dirListing: null,
      recentDirs: [],
      send: () => {},
      onSwitchCwd: () => {},
      onDone: () => {},
    });
    const body = el.querySelector(".overflow-y-auto");
    expect(body, "desktop body has overflow-y-auto").not.toBeNull();
  });
});
