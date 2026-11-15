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

  it("user time is self-end; agent time is self-start", async () => {
    const { el, unmount } = await renderPhone(phoneMsg({ role: "user", content: "hi" }));
    try {
      const userTime = el.querySelector("button.tabular-nums");
      expect(userTime!.classList.contains("self-end")).toBe(true);
    } finally {
      await unmount();
    }

    const { el: el2, unmount: unmount2 } = await renderPhone(phoneMsg({ content: "answer" }));
    try {
      const agentTime = el2.querySelector("button.tabular-nums");
      expect(agentTime!.classList.contains("self-start")).toBe(true);
    } finally {
      await unmount2();
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
      const timeIdx = children.indexOf(time!);
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
