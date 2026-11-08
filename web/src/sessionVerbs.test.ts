// Tests for `sessionVerbs.ts` — the pure `routeSessionVerb` router (CONTRACT 1).
//
// Written FIRST, expected to be RED until `web/src/sessionVerbs.ts` exists:
// the import `./sessionVerbs` does not resolve, so this file fails at
// collection for the RIGHT reason (feature absent), not a setup bug.
//
// Contract (locked):
//   export type VerbAction =
//     | { kind: "send" }
//     | { kind: "cd"; path: string }
//     | { kind: "switch_cwd"; path: string }
//     | { kind: "load"; convo: string }
//     | { kind: "refuse"; message: string };
//   export function routeSessionVerb(text: string, ctx: { busy: boolean; cwd: string }): VerbAction;
//
// Parsing: trim the text, match /^\/(\S+)(?:\s+([\s\S]*))?$/, lowercase the
// verb, trim the arg.
//   - `/cd <arg>`  → cd with an absolute path, both busy and idle. Absolute
//     means the arg matches /^([/\\~]|[A-Za-z]:)/ and is kept as typed;
//     otherwise the result is cwd + "/" + arg, with no doubled separator when
//     cwd ends in / or \.
//   - bare `/cd`   → send, both busy and idle.
//   - `/new` (exact, any case) → busy: switch_cwd with path = ctx.cwd;
//     idle: send.
//   - `/load <UUID>` → busy: load with that convo; idle: send.
//     `/load <anything else>` → busy: refuse with the exact message
//     "/load by index needs the turn to finish — press Stop first."; idle: send.
//   - Anything else (plain text, /model x, etc.) → send.
//
// Runs under `node`: the function takes plain data, no DOM needed.

import { describe, expect, it } from "vitest";
import { routeSessionVerb } from "./sessionVerbs";

type Ctx = { busy: boolean; cwd: string };
const idle = (cwd = "/a"): Ctx => ({ busy: false, cwd });
const busy = (cwd = "/a"): Ctx => ({ busy: true, cwd });

const REFUSE = "/load by index needs the turn to finish — press Stop first.";
const UUID = "77777777-7777-7777-7777-777777777777";

// ─── /cd (absolute, busy and idle) ──────────────────────────────────────

describe("routeSessionVerb — /cd with an absolute arg", () => {
  it("busy /cd /x → cd /x (case 1)", () => {
    expect(routeSessionVerb("/cd /x", busy("/a"))).toEqual({ kind: "cd", path: "/x" });
  });

  it("idle /cd /x → cd /x (case 1: same result idle)", () => {
    expect(routeSessionVerb("/cd /x", idle("/a"))).toEqual({ kind: "cd", path: "/x" });
  });

  it("cd is independent of busy (busy and idle agree)", () => {
    expect(routeSessionVerb("/cd /x", busy("/a"))).toEqual(routeSessionVerb("/cd /x", idle("/a")));
  });

  it("leading/trailing whitespace is trimmed before routing", () => {
    expect(routeSessionVerb("   /cd /x   ", busy("/a"))).toEqual({ kind: "cd", path: "/x" });
    expect(routeSessionVerb("  /cd   /x  ", idle("/a"))).toEqual({ kind: "cd", path: "/x" });
  });
});

// ─── /cd (relative resolution, no doubled separator) ─────────────────────

describe("routeSessionVerb — /cd relative arg (case 2)", () => {
  it("cwd /a + /cd b → cd /a/b", () => {
    expect(routeSessionVerb("/cd b", idle("/a"))).toEqual({ kind: "cd", path: "/a/b" });
  });

  it("cwd /a/ + /cd b → cd /a/b (no doubled /)", () => {
    expect(routeSessionVerb("/cd b", idle("/a/"))).toEqual({ kind: "cd", path: "/a/b" });
  });

  it("cwd C:\\a\\ + /cd b → cd C:\\a\\b (no doubled \\)", () => {
    // The contract says "no doubled separator when cwd ends in / or \".
    expect(routeSessionVerb("/cd b", idle("C:\\a\\"))).toEqual({
      kind: "cd",
      path: "C:\\a\\b",
    });
  });

  it("~/q stays as typed (absolute)", () => {
    expect(routeSessionVerb("/cd ~/q", busy("/a"))).toEqual({ kind: "cd", path: "~/q" });
  });

  it("C:\\w stays as typed (absolute, drive letter)", () => {
    expect(routeSessionVerb("/cd C:\\w", busy("/a"))).toEqual({ kind: "cd", path: "C:\\w" });
  });

  it("/abs stays as typed (absolute, leading slash)", () => {
    expect(routeSessionVerb("/cd /abs", idle("/a"))).toEqual({ kind: "cd", path: "/abs" });
  });

  it("a relative arg keeps busy-agnostic behaviour", () => {
    expect(routeSessionVerb("/cd b", busy("/a"))).toEqual({ kind: "cd", path: "/a/b" });
  });
});

// ─── bare /cd ────────────────────────────────────────────────────────────

describe("routeSessionVerb — bare /cd (case 3)", () => {
  it("bare /cd → send when busy", () => {
    expect(routeSessionVerb("/cd", busy("/a"))).toEqual({ kind: "send" });
  });

  it("bare /cd → send when idle", () => {
    expect(routeSessionVerb("/cd", idle("/a"))).toEqual({ kind: "send" });
  });

  it("bare /cd with trailing spaces → send", () => {
    expect(routeSessionVerb("/cd   ", busy("/a"))).toEqual({ kind: "send" });
  });
});

// ─── /new ────────────────────────────────────────────────────────────────

describe("routeSessionVerb — /new (case 4)", () => {
  it("busy /new → switch_cwd with ctx.cwd", () => {
    expect(routeSessionVerb("/new", busy("/a"))).toEqual({ kind: "switch_cwd", path: "/a" });
  });

  it("busy /NEW (case-insensitive) → switch_cwd with ctx.cwd", () => {
    expect(routeSessionVerb("/NEW", busy("/a"))).toEqual({ kind: "switch_cwd", path: "/a" });
  });

  it("idle /new → send", () => {
    expect(routeSessionVerb("/new", idle("/a"))).toEqual({ kind: "send" });
  });

  it("idle /NEW (case-insensitive) → send", () => {
    expect(routeSessionVerb("/NEW", idle("/a"))).toEqual({ kind: "send" });
  });

  it("busy /new with trailing whitespace → switch_cwd (trim first)", () => {
    expect(routeSessionVerb("/new  ", busy("/a"))).toEqual({ kind: "switch_cwd", path: "/a" });
  });

  it("busy /new <extra> is NOT the exact verb → send", () => {
    expect(routeSessionVerb("/new extra", busy("/a"))).toEqual({ kind: "send" });
  });
});

// ─── /load ───────────────────────────────────────────────────────────────

describe("routeSessionVerb — /load (case 5)", () => {
  it("busy /load <uuid> → load with that convo", () => {
    expect(routeSessionVerb(`/load ${UUID}`, busy("/a"))).toEqual({
      kind: "load",
      convo: UUID,
    });
  });

  it("busy /load 3 → refuse with the exact message", () => {
    expect(routeSessionVerb("/load 3", busy("/a"))).toEqual({ kind: "refuse", message: REFUSE });
  });

  it("busy /load foo → refuse with the exact message", () => {
    expect(routeSessionVerb("/load foo", busy("/a"))).toEqual({ kind: "refuse", message: REFUSE });
  });

  it("idle /load <uuid> → send", () => {
    expect(routeSessionVerb(`/load ${UUID}`, idle("/a"))).toEqual({ kind: "send" });
  });

  it("idle /load 3 → send (the refuse is busy-only)", () => {
    expect(routeSessionVerb("/load 3", idle("/a"))).toEqual({ kind: "send" });
  });

  it("busy /load (bare, no arg) → refuse with the exact message", () => {
    expect(routeSessionVerb("/load", busy("/a"))).toEqual({ kind: "refuse", message: REFUSE });
  });

  it("idle /load (bare, no arg) → send", () => {
    expect(routeSessionVerb("/load", idle("/a"))).toEqual({ kind: "send" });
  });
});

// ─── anything else → send ────────────────────────────────────────────────

describe("routeSessionVerb — anything else → send (case 6)", () => {
  it("busy plain text 'hello' → send", () => {
    expect(routeSessionVerb("hello", busy("/a"))).toEqual({ kind: "send" });
  });

  it("idle plain text 'hello' → send", () => {
    expect(routeSessionVerb("hello", idle("/a"))).toEqual({ kind: "send" });
  });

  it("busy /model x → send (a session verb we do not intercept)", () => {
    expect(routeSessionVerb("/model x", busy("/a"))).toEqual({ kind: "send" });
  });

  it("busy /stop → send (unclaimed slash command)", () => {
    expect(routeSessionVerb("/stop", busy("/a"))).toEqual({ kind: "send" });
  });

  it("empty string → send", () => {
    expect(routeSessionVerb("", busy("/a"))).toEqual({ kind: "send" });
  });

  it("whitespace-only string → send", () => {
    expect(routeSessionVerb("   ", busy("/a"))).toEqual({ kind: "send" });
  });

  it("a lone slash → send (no verb to match)", () => {
    expect(routeSessionVerb("/", busy("/a"))).toEqual({ kind: "send" });
  });

  it("unicode plain text → send", () => {
    expect(routeSessionVerb("héllo wörld — 你好", busy("/a"))).toEqual({ kind: "send" });
  });

  it("text with a leading slash that is not a known verb → send", () => {
    expect(routeSessionVerb("/compact now", busy("/a"))).toEqual({ kind: "send" });
  });
});
