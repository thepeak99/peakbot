import { describe, it, expect } from "vitest";
import { sameMessage } from "./Message";
import type { ChatMessage } from "../types";

// A canonical full message. Every test below mutates exactly one field off this
// base so each "differs in exactly one field" case is auditable.
function full(): ChatMessage {
  return {
    role: "agent",
    content: "hello world",
    timestamp: "10:14",
    toolName: "file_read",
    fromBackground: false,
    subAgentRole: undefined,
  };
}

describe("sameMessage", () => {
  it("returns true for two separately-constructed objects with equal values", () => {
    // Value comparison, not reference: same shape, distinct objects.
    expect(sameMessage(full(), full())).toBe(true);
  });

  it("returns true for the same reference (identity implies equality)", () => {
    const m = full();
    expect(sameMessage(m, m)).toBe(true);
  });

  it("returns false when role differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, role: "user" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when content differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, content: "different text" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when timestamp differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, timestamp: "10:15" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when toolName differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, toolName: "file_write" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when fromBackground differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, fromBackground: true };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when subAgentRole differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, subAgentRole: "junior" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when thinking is added to one but not the other", () => {
    // An assistant turn newly populated with thinking blocks must re-render
    // (the `<details>` flips from absent to present). Reference inequality is
    // the easy case here; the structural case below is the harder lock.
    const a = full();
    const b: ChatMessage = { ...a, thinking: ["step 1", "step 2"] };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when thinking arrays differ in content", () => {
    // Two messages with structurally equal shape but different thinking text
    // must not be considered the same — the DOM would have stale text inside
    // the collapsed <details>.
    const a: ChatMessage = { ...full(), thinking: ["alpha", "beta"] };
    const b: ChatMessage = { ...full(), thinking: ["alpha", "gamma"] };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when thinking arrays differ in length", () => {
    const a: ChatMessage = { ...full(), thinking: ["only"] };
    const b: ChatMessage = { ...full(), thinking: ["only", "extra"] };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns true when thinking arrays are structurally equal but distinct references", () => {
    // adaptMessage allocates a fresh array per call, so two equal transcripts
    // must compare equal even though the array references differ. This is the
    // property memo relies on for the thinking path.
    const a: ChatMessage = { ...full(), thinking: ["x", "y"] };
    const b: ChatMessage = { ...full(), thinking: ["x", "y"] };
    expect(a.thinking).not.toBe(b.thinking);
    expect(sameMessage(a, b)).toBe(true);
  });

  it("returns true when thinking is undefined vs absent (loose equality)", () => {
    // Mirrors the existing "optional fields undefined vs absent" case for the
    // new field: `thinking: undefined` and a message without the field at all
    // describe the same DOM (no <details> rendered).
    const withUndefined: ChatMessage = { ...full(), thinking: undefined };
    const absent: ChatMessage = { ...full() };
    expect(sameMessage(withUndefined, absent)).toBe(true);
  });

  it("returns true when optional fields are undefined vs absent (loose equality)", () => {
    // Implementation choice (documented): an optional field explicitly set to
    // `undefined` is equivalent to the field being absent. Both messages below
    // describe the same shape; sameMessage must return true. If a future change
    // wants stricter semantics, update this case alongside it.
    const withUndefined: ChatMessage = {
      role: "user",
      content: "hi",
      timestamp: "10:14",
      toolName: undefined,
      fromBackground: undefined,
      subAgentRole: undefined,
    };
    const absent: ChatMessage = {
      role: "user",
      content: "hi",
      timestamp: "10:14",
    };
    expect(sameMessage(withUndefined, absent)).toBe(true);
  });

  it("returns true for two minimal messages with only the required fields", () => {
    const a: ChatMessage = { role: "system", content: "ready", timestamp: "10:14" };
    const b: ChatMessage = { role: "system", content: "ready", timestamp: "10:14" };
    expect(sameMessage(a, b)).toBe(true);
  });
});
