// Focus-mode core rule (plan §4): which transcript rows survive, and which
// step feeds the activity row. Pure TS, no React — `Transcript` runs
// `adaptMessage` on the very objects we return, so every visible row must be
// the input's own reference.

import type { WireChatMessage } from "./state";

export interface FocusView {
  /** Messages to render in the transcript, in original order. */
  visible: WireChatMessage[];
  /** Newest hidden step of the in-flight turn; null when idle or nothing yet. */
  activity: WireChatMessage | null;
}

// No wire severity field exists; every current error emitter starts with
// ❌/⚠ — a future `level` on the wire is the clean fix (plan §8.1).
const ERROR_RE = /^\s*(❌|⚠)/;

function isError(m: WireChatMessage): boolean {
  return m.role === "system" && ERROR_RE.test(m.content ?? "");
}

export function focusView(
  messages: WireChatMessage[],
  isRunning: boolean,
): FocusView {
  const visible: WireChatMessage[] = [];
  let activity: WireChatMessage | null = null;
  let turn: WireChatMessage[] = [];

  // A turn is only classifiable once it is complete (the answer and the last
  // work index are end-of-turn facts), so buffer it and flush at the next
  // user message — or at the end, where isRunning decides.
  const flush = (running: boolean): void => {
    if (turn.length === 0) return;
    const isBackground = turn[0].source?.kind === "background";
    let lastWorkIdx = 0;
    let answerIdx = -1;
    for (let i = 1; i < turn.length; i++) {
      const m = turn[i];
      if (m.role !== "system") lastWorkIdx = i;
      if (m.role === "agent" && m.source?.kind !== "sub_agent" && (m.content ?? "").trim() !== "") {
        answerIdx = i;
      }
    }
    for (let i = 0; i < turn.length; i++) {
      const m = turn[i];
      const show = isError(m)
        ? true
        : isBackground
          ? !running && m.role === "system" && i > lastWorkIdx
          : running
            ? i === 0
            : i === 0 || i === answerIdx || (m.role === "system" && i > lastWorkIdx);
      if (show) visible.push(m);
      else if (running && i > 0) activity = m; // last hidden step wins
    }
  };

  for (const m of messages) {
    if (m.role === "user") {
      flush(false);
      turn = [m];
    } else if (turn.length === 0) {
      visible.push(m); // preamble (banners, /model lists) — visible unconditionally
    } else {
      turn.push(m);
    }
  }
  flush(isRunning);

  return { visible, activity };
}
