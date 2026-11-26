// Batch 3 phone contracts (fix-plan §4 BATCH 3, items 3.1/3.3–3.6): with
// `phone: true`, `Message` renders the phone faces — the user bubble (B11),
// the labelless agent prose (B12), the collapsible tool row (B13) and the
// code block with header + Copy button (B14) — and the desktop card is
// untouched when `phone` is absent (R1 lock).
//
// `Message` exists and is imported as-is; `phone` is passed through a typed
// alias so the file typechecks before the prop lands (3.1) and every new
// assertion fails on the DOM, not on an import.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createElement, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { Message } from "./Message";
import { formatClock, formatFull, localeClock } from "../dates";
import type { ChatMessage } from "../types";

// X2 adds `phone?: boolean` to Message's props; until it lands the cast keeps
// `tsc --noEmit` clean while the assertions below stay RED (feature absent).
const PhoneMessage = Message as unknown as ComponentType<{
  message: ChatMessage;
  phone?: boolean;
}>;

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  document.body.innerHTML = "";
});

const MSG_AT = 1732000000000; // fixed epoch for deterministic tests

function msg(over: Partial<ChatMessage> = {}): ChatMessage {
  return {
    role: "agent",
    content: "hello",
    at: MSG_AT,
    ...over,
  };
}

async function renderPhone(message: ChatMessage): Promise<{
  el: HTMLElement;
  unmount: () => Promise<void>;
}> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(createElement(PhoneMessage, { message, phone: true }));
  });
  return {
    el,
    unmount: async () => {
      await act(async () => {
        root.unmount();
      });
    },
  };
}

// Same, without `phone` — the desktop lock (R1).
async function renderDesktop(message: ChatMessage): Promise<{
  el: HTMLElement;
  unmount: () => Promise<void>;
}> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(createElement(Message, { message }));
  });
  return {
    el,
    unmount: async () => {
      await act(async () => {
        root.unmount();
      });
    },
  };
}

let writeText: ReturnType<typeof vi.fn>;
beforeEach(() => {
  writeText = vi.fn();
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
});

describe("Message — phone (phone: true)", () => {
  it("user: no 'You' label and no timestamp; the text renders in the bubble (B11)", async () => {
    const { el, unmount } = await renderPhone(
      msg({ role: "user", content: "hello there" }),
    );
    try {
      expect(el.textContent, "no role label on the phone").not.toContain("You");
      expect(el.textContent, "no timestamp on the phone").not.toContain("10:14");
      expect(el.textContent).toContain("hello there");
      // The bubble face is the class contract (B11): 88%-width, zinc-800,
      // the asymmetric 18/18/6/18 radius.
      const bubble = [...el.querySelectorAll("div")].find(
        (d) =>
          d.textContent === "hello there" &&
          d.classList.contains("bg-zinc-800") &&
          d.classList.contains("rounded-[18px_18px_6px_18px]"),
      );
      expect(bubble, "the user bubble carries the B11 face").toBeDefined();
    } finally {
      await unmount();
    }
  });

  it("agent: no 'Shifu' label and no timestamp; markdown renders (B12)", async () => {
    const { el, unmount } = await renderPhone(
      msg({ content: "The answer is **bold**." }),
    );
    try {
      expect(el.textContent, "no role label on the phone").not.toContain("Shifu");
      expect(el.textContent, "no timestamp on the phone").not.toContain("10:14");
      expect(el.querySelector("strong")?.textContent, "markdown renders").toBe("bold");
    } finally {
      await unmount();
    }
  });

  it("a sub-agent agent message still shows '🧩 ‹role›' (B12 keep-R)", async () => {
    const { el, unmount } = await renderPhone(
      msg({ content: "done digging", subAgentRole: "researcher" }),
    );
    try {
      expect(el.textContent).toContain("🧩 researcher");
    } finally {
      await unmount();
    }
  });

  it("a background agent message keeps the '🛰 bg' badge (B12 keep-R)", async () => {
    const { el, unmount } = await renderPhone(
      msg({ content: "night report", fromBackground: true }),
    );
    try {
      expect(el.textContent).toContain("🛰 bg");
    } finally {
      await unmount();
    }
  });

  it("tool call: collapsed by default (name + first line + chevron); a tap expands to the full content; a second tap collapses (B13)", async () => {
    const { el, unmount } = await renderPhone(
      msg({
        role: "toolCall",
        toolName: "file_read",
        content: "Read /etc/hosts\nsecond line of the output",
      }),
    );
    try {
      const btn = el.querySelector<HTMLElement>('button[aria-expanded="false"]');
      expect(btn, "the tool row is a collapsed button by default").not.toBeNull();
      expect(btn!.querySelector("svg"), "the row carries a chevron").not.toBeNull();
      expect(el.textContent).toContain("file_read");
      expect(el.textContent).toContain("Read /etc/hosts");
      expect(
        el.textContent,
        "the collapsed row shows only the first line",
      ).not.toContain("second line of the output");
      expect(el.querySelector('[data-testid="tool-ext"]')).toBeNull();

      await act(async () => {
        btn!.click();
      });
      expect(btn!.getAttribute("aria-expanded")).toBe("true");
      const ext = el.querySelector('[data-testid="tool-ext"]');
      expect(ext, "expansion shows the tool-ext details").not.toBeNull();
      expect(ext!.textContent).toContain("second line of the output");

      await act(async () => {
        btn!.click();
      });
      expect(btn!.getAttribute("aria-expanded")).toBe("false");
      expect(el.querySelector('[data-testid="tool-ext"]')).toBeNull();
    } finally {
      await unmount();
    }
  });

  it("tool result: the same collapsed row expands to the full result (B13)", async () => {
    const { el, unmount } = await renderPhone(
      msg({
        role: "toolResult",
        toolName: "bash",
        content: "total 48\n-rw-r--r-- 1 exe exe 100 Jan 1 00:00 note.md",
      }),
    );
    try {
      const btn = el.querySelector<HTMLElement>('button[aria-expanded="false"]');
      expect(btn, "the result row is a collapsed button").not.toBeNull();
      expect(el.textContent).toContain("bash");
      expect(el.textContent).not.toContain("rw-r--r--");

      await act(async () => {
        btn!.click();
      });
      expect(btn!.getAttribute("aria-expanded")).toBe("true");
      expect(el.querySelector('[data-testid="tool-ext"]')?.textContent).toContain(
        "rw-r--r--",
      );
    } finally {
      await unmount();
    }
  });

  it("a ```rust fence renders a header reading 'rust' and a 'Copy code' button that writes the code text to the clipboard (B14)", async () => {
    const { el, unmount } = await renderPhone(
      msg({ content: "```rust\nlet x = 42;\n```" }),
    );
    try {
      const block = el.querySelector(".code-block");
      expect(block, "the phone code block wrapper").not.toBeNull();
      const hdr = block!.querySelector(".code-hdr");
      expect(hdr, "the code header").not.toBeNull();
      expect(hdr!.textContent, "the header names the language").toContain("rust");
      const copy = block!.querySelector<HTMLElement>('button[aria-label="Copy code"]');
      expect(copy, "the Copy code button").not.toBeNull();

      await act(async () => {
        copy!.click();
      });
      expect(writeText, "copying writes the code text").toHaveBeenCalledTimes(1);
      expect(writeText).toHaveBeenCalledWith("let x = 42;\n");
    } finally {
      await unmount();
    }
  });

  it("a ```diff fence marks added and removed lines (B14)", async () => {
    // The hljs-addition / hljs-deletion classes ARE the contract: the phone
    // diff tint (3.7) hooks off them.
    const { el, unmount } = await renderPhone(
      msg({ content: "```diff\n+added line\n-removed line\n context line\n```" }),
    );
    try {
      expect(el.querySelector(".hljs-addition"), "added lines are marked").not.toBeNull();
      expect(el.querySelector(".hljs-deletion"), "removed lines are marked").not.toBeNull();
    } finally {
      await unmount();
    }
  });
});

// ─── Phone timestamps (design §7.1) ──────────────────────────────────────
//
// Phone faces now show a tappable HH:MM under every message. Tapping toggles
// to the full date format. User/system time is self-end; agent/summary/tool
// time is self-start. Thinking-only messages have no empty prose box and the
// time is outside the <details> border.

function phoneMsg(over: Partial<ChatMessage> = {}): ChatMessage {
  return {
    role: "agent",
    content: "hello",
    at: MSG_AT,
    ...over,
  };
}

describe("Message — phone timestamps (design §7.1)", () => {
  it("shows formatClock(at) under user and agent messages", async () => {
    const { el, unmount } = await renderPhone(phoneMsg({ role: "user", content: "hi" }));
    try {
      expect(el.textContent).toContain(formatClock(MSG_AT));
    } finally {
      await unmount();
    }

    const { el: el2, unmount: unmount2 } = await renderPhone(phoneMsg({ content: "answer" }));
    try {
      expect(el2.textContent).toContain(formatClock(MSG_AT));
    } finally {
      await unmount2();
    }
  });

  it("tapping the time toggles to formatFull(at) and back", async () => {
    const { el, unmount } = await renderPhone(phoneMsg({ role: "user", content: "hi" }));
    try {
      const timeBtn = el.querySelector<HTMLButtonElement>("button.tabular-nums");
      expect(timeBtn, "time button renders").not.toBeNull();
      expect(timeBtn!.textContent).toContain(formatClock(MSG_AT));
      expect(timeBtn!.getAttribute("aria-expanded")).toBe("false");

      await act(async () => {
        timeBtn!.click();
      });
      expect(timeBtn!.textContent).toContain(formatFull(MSG_AT));
      expect(timeBtn!.getAttribute("aria-expanded")).toBe("true");

      await act(async () => {
        timeBtn!.click();
      });
      expect(timeBtn!.textContent).toContain(formatClock(MSG_AT));
      expect(timeBtn!.getAttribute("aria-expanded")).toBe("false");
    } finally {
      await unmount();
    }
  });

  // Updated for the PhoneMeta row: alignment moved OFF MessageTime and onto a
  // dedicated `data-testid="phone-meta"` row so the optional Copy message
  // button can sit beside the clock without either one owning the edge.
  it("user time row is self-end; agent time row is self-start — the phone-meta row owns alignment", async () => {
    const { el, unmount } = await renderPhone(phoneMsg({ role: "user", content: "hi" }));
    try {
      const userMeta = el.querySelector<HTMLElement>('[data-testid="phone-meta"]');
      expect(userMeta, "the PhoneMeta row renders under a user message").not.toBeNull();
      expect(userMeta!.classList.contains("self-end"), "user meta row is self-end").toBe(true);

      const userTime = userMeta!.querySelector<HTMLButtonElement>("button.tabular-nums");
      expect(userTime, "MessageTime lives inside the PhoneMeta row").not.toBeNull();
      expect(
        userTime!.classList.contains("self-end") ||
          userTime!.classList.contains("self-start"),
        "alignment is owned by the row, not duplicated on the time button",
      ).toBe(false);
    } finally {
      await unmount();
    }

    const { el: el2, unmount: unmount2 } = await renderPhone(phoneMsg({ content: "answer" }));
    try {
      const agentMeta = el2.querySelector<HTMLElement>('[data-testid="phone-meta"]');
      expect(agentMeta, "the PhoneMeta row renders under an agent message").not.toBeNull();
      expect(agentMeta!.classList.contains("self-start"), "agent meta row is self-start").toBe(true);

      const agentTime = agentMeta!.querySelector<HTMLButtonElement>("button.tabular-nums");
      expect(agentTime, "MessageTime lives inside the PhoneMeta row").not.toBeNull();
      expect(
        agentTime!.classList.contains("self-end") ||
          agentTime!.classList.contains("self-start"),
        "alignment is owned by the row, not duplicated on the time button",
      ).toBe(false);
    } finally {
      await unmount2();
    }
  });

  it("a tool row's phone-meta row is self-start and hosts the time button", async () => {
    const { el, unmount } = await renderPhone(
      phoneMsg({ role: "toolResult", toolName: "bash", content: "ok" }),
    );
    try {
      const meta = el.querySelector<HTMLElement>('[data-testid="phone-meta"]');
      expect(meta, "the tool row gets a PhoneMeta row").not.toBeNull();
      expect(meta!.classList.contains("self-start"), "tool meta row is self-start").toBe(true);
      expect(meta!.querySelector("button.tabular-nums"), "the time lives in the row").not.toBeNull();
    } finally {
      await unmount();
    }
  });

  it("thinking-only: no .markdown-body element, time not inside <details>", async () => {
    const { el, unmount } = await renderPhone(
      phoneMsg({ role: "agent", content: "", thinking: ["thinking step"] }),
    );
    try {
      expect(el.querySelector(".markdown-body")).toBeNull();
      const details = el.querySelector("details");
      expect(details, "thinking <details> renders").not.toBeNull();
      const time = el.querySelector("button.tabular-nums");
      expect(time, "time renders").not.toBeNull();
      expect(details!.contains(time!), "time is outside <details>").toBe(false);
    } finally {
      await unmount();
    }
  });

  it("thinking + content: order is details → prose → time", async () => {
    const { el, unmount } = await renderPhone(
      phoneMsg({
        role: "agent",
        content: "the answer",
        thinking: ["thinking step"],
      }),
    );
    try {
      const details = el.querySelector("details");
      const prose = el.querySelector(".markdown-body");
      const time = el.querySelector("button.tabular-nums");
      expect(details).not.toBeNull();
      expect(prose).not.toBeNull();
      expect(time).not.toBeNull();
      // In jsdom, getBoundingClientRect returns 0s, so check source order via
      // the elements' position in the parent's children
      const parent = prose!.parentElement!;
      const children = [...parent.children];
      const detailsIdx = children.indexOf(details!);
      const proseIdx = children.indexOf(prose!);
      // The tappable time now lives inside the `data-testid="phone-meta"` row
      // (see the PhoneMeta contract), so its direct sibling is that row; the
      // document order under test is unchanged: details → prose → time.
      const timeIdx = children.indexOf(time!.parentElement!);
      expect(detailsIdx).toBeLessThan(proseIdx);
      expect(proseIdx).toBeLessThan(timeIdx);
    } finally {
      await unmount();
    }
  });
});

describe("Message — desktop lock (no phone, R1)", () => {
  it("the desktop agent card still shows the role label and timestamp", async () => {
    const { el, unmount } = await renderDesktop(
      msg({ content: "The answer is **bold**." }),
    );
    try {
      expect(el.textContent).toContain("Shifu");
      expect(el.textContent).toContain(localeClock(MSG_AT));
    } finally {
      await unmount();
    }
  });

  it("the desktop user card still shows the 'You' label and timestamp", async () => {
    const { el, unmount } = await renderDesktop(
      msg({ role: "user", content: "hello there" }),
    );
    try {
      expect(el.textContent).toContain("You");
      expect(el.textContent).toContain(localeClock(MSG_AT));
    } finally {
      await unmount();
    }
  });
});

// ─── Phone tool-row image rendering (fix-plan: phone ToolRow renders images) ───
//
// The phone face's ToolRow previously rendered expanded toolResult content as
// literal text (no markdown parsing), so a view_image result like
// `![shot.png](/images/<64hex>.png)` appeared as raw markdown text, not as a
// CachedImage <img>. The fix parses toolResult content as markdown (reusing
// PHONE_MARKDOWN_COMPONENTS) while leaving toolCall bodies literal.
//
// Contract (locked design):
// - Collapsed tool row: compact, no <img> element
// - Expanded toolResult row: markdown-rendered, CachedImage shows as lazy <img>
// - Clicking the expanded <img> opens the Lightbox
// - Remote relay URLs (/r/<profile>/images/<id>) render exactly as-is
// - Non-renderable local paths get the 🖼 fallback (not an <img>)
// - Raw HTML <img> tags do not create real <img> elements (no injection)
// - Multiple markdown image lines produce multiple <img> elements in order
// - toolCall bodies stay literal (no markdown strong/code interpretation)

const IMG_ID = `${"a".repeat(64)}.png`;
const REMOTE_IMG_ID = `${"b".repeat(64)}.png`;

function toolResultMsg(content: string): ChatMessage {
  return msg({ role: "toolResult", toolName: "bash", content });
}

function toolCallMsg(content: string): ChatMessage {
  return msg({ role: "toolCall", toolName: "view_image", content });
}

async function expandToolRow(el: HTMLElement) {
  const btn = el.querySelector<HTMLElement>('button[aria-expanded="false"]');
  expect(btn, "tool row should be collapsed by default").not.toBeNull();
  await act(async () => {
    btn!.click();
  });
  expect(btn!.getAttribute("aria-expanded")).toBe("true");
}

describe("Message — phone tool-row image rendering", () => {
  it("collapsed: no <img>; expanded: lazy <img> with exact src, no literal ![](", async () => {
    const src = `/images/${IMG_ID}`;
    const { el, unmount } = await renderPhone(
      toolResultMsg(`![shot.png](${src})`),
    );
    try {
      expect(el.querySelector("img"), "collapsed row must not render an <img>").toBeNull();

      await expandToolRow(el);

      const img = el.querySelector("img");
      expect(img, "expanded row must render a CachedImage <img>").not.toBeNull();
      expect(img!.getAttribute("src")).toBe(src);
      expect(img!.getAttribute("loading")).toBe("lazy");
      const toolExt = el.querySelector('[data-testid="tool-ext"]');
      expect(toolExt, "expanded row must show the tool-ext details").not.toBeNull();
      expect(toolExt!.textContent, "expanded body must not show the literal markdown syntax").not.toContain(`![shot.png](${src})`);
    } finally {
      await unmount();
    }
  });

  it("clicking the expanded tool-row image opens the Lightbox", async () => {
    const src = `/images/${IMG_ID}`;
    const { el, unmount } = await renderPhone(
      toolResultMsg(`![shot.png](${src})`),
    );
    try {
      await expandToolRow(el);

      const img = el.querySelector("img");
      expect(img, "expanded row must render a CachedImage <img>").not.toBeNull();
      expect(document.querySelector('[role="dialog"]')).toBeNull();

      await act(async () => {
        img!.click();
      });

      const dialog = document.querySelector('[role="dialog"][aria-modal="true"]');
      expect(dialog, "clicking the tool-row image must open the Lightbox").not.toBeNull();
    } finally {
      await unmount();
    }
  });

  it("remote relay URL /r/home-dev/images/<id> renders with exact src", async () => {
    const src = `/r/home-dev/images/${REMOTE_IMG_ID}`;
    const { el, unmount } = await renderPhone(
      toolResultMsg(`![shot.png](${src})`),
    );
    try {
      await expandToolRow(el);

      const img = el.querySelector("img");
      expect(img, "remote relay image must render as <img>").not.toBeNull();
      expect(img!.getAttribute("src")).toBe(src);
    } finally {
      await unmount();
    }
  });

  it("non-renderable local path /etc/x.png shows 🖼 fallback, not <img>", async () => {
    const { el, unmount } = await renderPhone(
      toolResultMsg("![x](/etc/x.png)"),
    );
    try {
      await expandToolRow(el);

      expect(el.querySelector("img"), "bare local path must not render an <img>").toBeNull();
      expect(el.textContent).toContain("🖼 x");
    } finally {
      await unmount();
    }
  });

  it("raw HTML <img> tag in bash tool result does not create a real <img> element", async () => {
    const { el, unmount } = await renderPhone(
      toolResultMsg('<img src="http://example.com/x.png" onerror="alert(1)">'),
    );
    try {
      await expandToolRow(el);

      expect(el.querySelector("img"), "raw HTML <img> must not create a real <img> element").toBeNull();
    } finally {
      await unmount();
    }
  });

  it("two markdown image lines produce two <img> elements in order", async () => {
    const src1 = `/images/${IMG_ID}`;
    const src2 = `/images/${REMOTE_IMG_ID}`;
    const { el, unmount } = await renderPhone(
      toolResultMsg(`![first](${src1})\n![second](${src2})`),
    );
    try {
      await expandToolRow(el);

      const imgs = Array.from(el.querySelectorAll("img"));
      expect(imgs, "two markdown image lines must produce two <img> elements").toHaveLength(2);
      expect(imgs[0].getAttribute("src")).toBe(src1);
      expect(imgs[1].getAttribute("src")).toBe(src2);
    } finally {
      await unmount();
    }
  });

  it("toolCall body is literal: **bold** and `code` stay as text, no <strong> or <code>", async () => {
    const { el, unmount } = await renderPhone(
      toolCallMsg("read **bold** and `code`"),
    );
    try {
      await expandToolRow(el);

      expect(el.querySelector("strong"), "toolCall body must not interpret markdown strong").toBeNull();
      expect(el.querySelector("code"), "toolCall body must not interpret markdown code").toBeNull();
      expect(el.textContent).toContain("**bold**");
      expect(el.textContent).toContain("`code`");
    } finally {
      await unmount();
    }
  });
});

// ─── Phone message copy (shared CopyButton) ───────────────────────────────
//
// Contract: exactly one icon-only `aria-label="Copy message"` button per
// NONEMPTY message body — user bubbles, agent/system/summary prose, and
// toolCall/toolResult rows (even while the row is collapsed). The button
// copies `message.content` RAW (markdown source included), read at click
// time. Whitespace-only content gets no copy button. The existing fence
// `Copy code` button is separate and must not be duplicated.
//
// `writeText` is installed by the file-level beforeEach above.

function copyMessageButtons(el: HTMLElement): HTMLButtonElement[] {
  return Array.from(el.querySelectorAll<HTMLButtonElement>('button[aria-label="Copy message"]'));
}

function copyCodeButtons(el: HTMLElement): HTMLButtonElement[] {
  return Array.from(el.querySelectorAll<HTMLButtonElement>('button[aria-label="Copy code"]'));
}

/** The collapsed tool row's toggle. Selected by aria-expanded + a chevron svg
 * so the MessageTime button (also aria-expanded) is never picked up. */
function toolRowButton(el: HTMLElement): HTMLButtonElement | null {
  return (
    Array.from(el.querySelectorAll<HTMLButtonElement>("button")).find(
      (b) => b.getAttribute("aria-expanded") === "false" && b.querySelector("svg"),
    ) ?? null
  );
}

async function clickCopy(btn: HTMLButtonElement): Promise<void> {
  await act(async () => {
    btn.click();
  });
}

describe("Message — phone copy message", () => {
  it("agent prose: exactly one 'Copy message' button writes the raw markdown source", async () => {
    const content = "**bold** and *em*\n\n```js\nlet a = 1;\n```";
    const { el, unmount } = await renderPhone(msg({ content }));
    try {
      const copies = copyMessageButtons(el);
      expect(copies, "one Copy message button for the agent prose").toHaveLength(1);

      await clickCopy(copies[0]);
      expect(writeText).toHaveBeenCalledTimes(1);
      expect(writeText, "raw markdown, not the rendered text").toHaveBeenCalledWith(content);
      expect(writeText).not.toHaveBeenCalledWith("bold and em");
    } finally {
      await unmount();
    }
  });

  it("user message: exactly one 'Copy message' button writes the raw user text", async () => {
    const content = "  keep my **markdown** and   spaces  ";
    const { el, unmount } = await renderPhone(msg({ role: "user", content }));
    try {
      const copies = copyMessageButtons(el);
      expect(copies, "one Copy message button for the user bubble").toHaveLength(1);

      await clickCopy(copies[0]);
      expect(writeText).toHaveBeenCalledWith(content);
    } finally {
      await unmount();
    }
  });

  it("system + summary prose: one 'Copy message' button each, writing the raw banner text", async () => {
    for (const role of ["system", "summary"] as const) {
      const content = `a ${role} banner\nwith a second line`;
      const { el, unmount } = await renderPhone(msg({ role, content }));
      try {
        const copies = copyMessageButtons(el);
        expect(copies, `one Copy message button for role=${role}`).toHaveLength(1);
        await clickCopy(copies[0]);
        expect(writeText, `raw text for role=${role}`).toHaveBeenCalledWith(content);
      } finally {
        await unmount();
        writeText.mockClear();
      }
    }
  });

  it("collapsed toolCall: one 'Copy message' button writes the FULL content, not the first line", async () => {
    const content = "Read /etc/hosts\nsecond line of the output";
    const { el, unmount } = await renderPhone(
      msg({ role: "toolCall", toolName: "file_read", content }),
    );
    try {
      expect(
        el.querySelector('[data-testid="tool-ext"]'),
        "the row stays collapsed for this contract",
      ).toBeNull();
      const copies = copyMessageButtons(el);
      expect(copies, "one Copy message button on the collapsed toolCall row").toHaveLength(1);

      await clickCopy(copies[0]);
      expect(writeText, "copy is not limited to the collapsed first line").toHaveBeenCalledWith(
        content,
      );
    } finally {
      await unmount();
    }
  });

  it("collapsed toolResult: one 'Copy message' button writes the FULL result", async () => {
    const content = "total 48\n-rw-r--r-- 1 exe exe 100 Jan 1 00:00 note.md";
    const { el, unmount } = await renderPhone(
      msg({ role: "toolResult", toolName: "bash", content }),
    );
    try {
      expect(el.querySelector('[data-testid="tool-ext"]')).toBeNull();
      const copies = copyMessageButtons(el);
      expect(copies, "one Copy message button on the collapsed toolResult row").toHaveLength(1);

      await clickCopy(copies[0]);
      expect(writeText).toHaveBeenCalledWith(content);
    } finally {
      await unmount();
    }
  });

  it("whitespace-only content gets NO 'Copy message' button (user, agent, toolCall, toolResult)", async () => {
    const blank = "   \n\t  ";
    for (const role of ["user", "agent", "toolCall", "toolResult"] as const) {
      const { el, unmount } = await renderPhone(msg({ role, content: blank }));
      try {
        expect(copyMessageButtons(el), `no Copy message button for role=${role}`).toHaveLength(0);
      } finally {
        await unmount();
      }
    }
  });

  it("a fenced message has one 'Copy message' and one 'Copy code', with distinct payloads", async () => {
    const content = "intro text\n\n```rust\nlet x = 42;\n```";
    const { el, unmount } = await renderPhone(msg({ content }));
    try {
      const messageCopies = copyMessageButtons(el);
      const codeCopies = copyCodeButtons(el);
      expect(messageCopies, "one Copy message button").toHaveLength(1);
      expect(codeCopies, "the existing Copy code button is not duplicated").toHaveLength(1);
      expect(
        messageCopies[0],
        "the two affordances are distinct buttons",
      ).not.toBe(codeCopies[0]);

      await clickCopy(codeCopies[0]);
      expect(writeText, "Copy code writes the rendered pre text").toHaveBeenCalledWith(
        "let x = 42;\n",
      );

      writeText.mockClear();
      await clickCopy(messageCopies[0]);
      expect(writeText, "Copy message writes the full raw markdown source").toHaveBeenCalledWith(
        content,
      );
    } finally {
      await unmount();
    }
  });

  it("the phone-meta row hosts the Copy message button beside the time", async () => {
    const { el, unmount } = await renderPhone(
      msg({ role: "user", content: "hi there" }),
    );
    try {
      const meta = el.querySelector<HTMLElement>('[data-testid="phone-meta"]');
      expect(meta, "the PhoneMeta row renders").not.toBeNull();
      expect(
        meta!.querySelector('button[aria-label="Copy message"]'),
        "Copy message sits in the PhoneMeta row",
      ).not.toBeNull();
      expect(
        meta!.querySelector("button.tabular-nums"),
        "the time button shares the row",
      ).not.toBeNull();
    } finally {
      await unmount();
    }

    const { el: el2, unmount: unmount2 } = await renderPhone(
      msg({ role: "toolResult", toolName: "bash", content: "ok" }),
    );
    try {
      const meta = el2.querySelector<HTMLElement>('[data-testid="phone-meta"]');
      expect(meta, "the tool row also uses the PhoneMeta row").not.toBeNull();
      expect(
        meta!.querySelector('button[aria-label="Copy message"]'),
        "the collapsed tool row's copy lives in the PhoneMeta row",
      ).not.toBeNull();
    } finally {
      await unmount2();
    }
  });
});

// ─── Mobile tool attribution ──────────────────────────────────────────────
//
// ToolRow shows `🧩 <subAgentRole>` exactly once inside its collapsed toggle
// button (both toolCall and toolResult), tagged data-testid="tool-agent-role".
// No chip when subAgentRole is absent. Focus filtering is not involved here:
// rows are rendered directly with focus mode off.

function roleChips(el: HTMLElement): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>('[data-testid="tool-agent-role"]'));
}

describe("Message — phone tool attribution", () => {
  it("toolCall: one '🧩 researcher' chip inside the collapsed row button", async () => {
    const { el, unmount } = await renderPhone(
      msg({ role: "toolCall", toolName: "web_search", content: "query", subAgentRole: "researcher" }),
    );
    try {
      const row = toolRowButton(el);
      expect(row, "the collapsed tool row button renders").not.toBeNull();

      const chips = roleChips(el);
      expect(chips, "exactly one tool-agent-role chip").toHaveLength(1);
      expect(row!.contains(chips[0]), "the chip lives inside the collapsed row button").toBe(true);
      expect(chips[0].textContent?.trim()).toBe("🧩 researcher");
      expect((el.textContent ?? "").split("🧩 researcher")).toHaveLength(2);
    } finally {
      await unmount();
    }
  });

  it("toolResult: one '🧩 researcher' chip inside the collapsed row button", async () => {
    const { el, unmount } = await renderPhone(
      msg({ role: "toolResult", toolName: "bash", content: "ok", subAgentRole: "researcher" }),
    );
    try {
      const row = toolRowButton(el);
      expect(row, "the collapsed tool row button renders").not.toBeNull();

      const chips = roleChips(el);
      expect(chips, "exactly one tool-agent-role chip").toHaveLength(1);
      expect(row!.contains(chips[0]), "the chip lives inside the collapsed row button").toBe(true);
      expect(chips[0].textContent?.trim()).toBe("🧩 researcher");
    } finally {
      await unmount();
    }
  });

  it("no chip when subAgentRole is absent (both tool roles)", async () => {
    for (const role of ["toolCall", "toolResult"] as const) {
      const { el, unmount } = await renderPhone(
        msg({ role, toolName: "bash", content: "ok" }),
      );
      try {
        expect(roleChips(el), `no tool-agent-role chip for role=${role}`).toHaveLength(0);
        expect(el.textContent ?? "").not.toContain("🧩");
      } finally {
        await unmount();
      }
    }
  });

  it("a sub-agent prose message still labels its lane exactly once", async () => {
    const { el, unmount } = await renderPhone(
      msg({ content: "done digging", subAgentRole: "researcher" }),
    );
    try {
      expect(
        (el.textContent ?? "").split("🧩 researcher"),
        "the existing prose lane tag renders once, not duplicated by the tool chip",
      ).toHaveLength(2);
    } finally {
      await unmount();
    }
  });
});
