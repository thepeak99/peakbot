// @vitest-environment jsdom
//
// UI test: cached/offline rows must have an understandable stale indicator.
//
// Contract (design §4, user requirement):
//   - When a machine is offline, its cached rows (if shown in All mode) must
//     carry a visible indicator that they are stale/cached, not live.
//   - Without this, cached data is invisible and users cannot tell what is
//     current vs. stale.
//   - Live rows (machine online) and local rows must not carry the indicator.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { ChatList } from "./ChatList";
import type { ConversationSummary, WireMachineStatus } from "../state";
import type { CatalogProfile } from "../types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  vi.restoreAllMocks();
});

// ─── helpers ────────────────────────────────────────────────────────────

const HS = "https://home-server.local:7823";

const prof = (name: string, machine: string | null): CatalogProfile => ({
  name,
  origin: machine === null ? "global" : "remote",
  overridesGlobal: false,
  machine,
  agents: [],
});

function row(
  id: string,
  name: string,
  profile: string | null,
  updatedAt: string = "2026-01-05T12:00:00Z",
): ConversationSummary {
  return {
    id,
    name,
    updated_at: updatedAt,
    message_count: 3,
    model: "sonnet",
    active: false,
    running: false,
    profile,
  };
}

async function mountChatList(
  conversations: ConversationSummary[],
  catalog: CatalogProfile[],
  machineStatus: WireMachineStatus[] = [],
) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);

  await act(async () => {
    root.render(
      <ChatList
        conversations={conversations}
        catalog={catalog}
        loaded={true}
        unread={new Set()}
        connected={true}
        error={null}
        onOpen={vi.fn()}
        onNew={vi.fn()}
        machineStatus={machineStatus}
      />,
    );
  });

  return { container, root };
}

// ─── tests ──────────────────────────────────────────────────────────────

describe("cached/offline row indicators", () => {
  // The single, precise marker for a cached/offline row. Earlier this file
  // used a broad OR-chain that ended in `.find()` (returns `undefined`, not
  // `null`), so `expect(...).not.toBeNull()` passed even when NO indicator was
  // rendered (D1). The contract is now one explicit element.
  const CACHED = "[data-testid='chat-cached']";

  function accessibleLabel(el: Element | null): string {
    return (
      el?.getAttribute("aria-label") ??
      el?.getAttribute("title") ??
      el?.textContent ??
      ""
    ).toLowerCase();
  }

  it("rows from offline machines show a stale indicator in All mode", async () => {
    const catalog = [
      prof("local", null),
      prof("remote", HS),
    ];

    // One live local row, one cached row from offline remote
    const conversations = [
      row("local-1", "Live local", "local", "2026-01-06T12:00:00Z"),
      row("remote-1", "Cached remote", "remote", "2026-01-05T12:00:00Z"),
    ];

    // Remote machine is OFFLINE
    const machineStatus = [{ id: HS, online: false }];

    const { container, root } = await mountChatList(conversations, catalog, machineStatus);

    // In All mode, the remote row should still be visible but indicated as stale
    const remoteRow = container.querySelector("[data-convo='remote-1']");
    expect(remoteRow).not.toBeNull();

    // A cached row from an offline origin carries an explicit indicator...
    const indicator = remoteRow!.querySelector(CACHED);
    expect(
      indicator,
      "Rows from offline machines must carry an explicit cached indicator",
    ).not.toBeNull();
    // ...exposed to assistive tech with a meaningful label (not a bare tag).
    expect(
      accessibleLabel(indicator),
      "the cached indicator must announce itself as cached/offline/stale",
    ).toMatch(/cached|offline|stale/);

    // The local live row must NOT be marked cached.
    expect(container.querySelector("[data-convo='local-1']")!.querySelector(CACHED)).toBeNull();

    root.unmount();
    container.remove();
  });

  it("live rows from online machines have no stale indicator", async () => {
    const catalog = [
      prof("local", null),
      prof("remote", HS),
    ];

    const conversations = [
      row("local-1", "Live local", "local", "2026-01-06T12:00:00Z"),
      row("remote-1", "Live remote", "remote", "2026-01-06T12:00:00Z"),
    ];

    // Remote machine is ONLINE
    const machineStatus = [{ id: HS, online: true }];

    const { container, root } = await mountChatList(conversations, catalog, machineStatus);

    const remoteRow = container.querySelector("[data-convo='remote-1']");
    expect(remoteRow).not.toBeNull();

    expect(
      remoteRow!.querySelector(CACHED),
      "Live rows from online machines must NOT carry a cached indicator",
    ).toBeNull();

    root.unmount();
    container.remove();
  });

  it("local rows (no machine) have no stale indicator", async () => {
    const catalog = [prof("local", null)];

    const conversations = [
      row("local-1", "Local conversation", "local", "2026-01-06T12:00:00Z"),
    ];

    const { container, root } = await mountChatList(conversations, catalog);

    const localRow = container.querySelector("[data-convo='local-1']");
    expect(localRow).not.toBeNull();

    expect(
      localRow!.querySelector(CACHED),
      "Local rows must NOT carry a cached indicator",
    ).toBeNull();

    root.unmount();
    container.remove();
  });
});
