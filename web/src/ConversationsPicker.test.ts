import { describe, it, expect } from "vitest";
import { filterConversations } from "./components/ConversationsPicker";
import type { ConversationSummary } from "./state";

function makeConvo(
  overrides: Partial<ConversationSummary> = {},
): ConversationSummary {
  return {
    id: "aaaa1111-bbbb-4ccc-8ddd-eeeeeeee0001",
    name: "Project Planning",
    updated_at: "2026-01-01T00:00:00Z",
    message_count: 5,
    model: "gpt-4o",
    active: false,
    ...overrides,
  };
}

const fixtures: ConversationSummary[] = [
  makeConvo({ id: "aaaa1111-bbbb-4ccc-8ddd-eeeeeeee0001", name: "Project Planning", model: "gpt-4o" }),
  makeConvo({ id: "bbbb2222-cccc-4ddd-9eee-ffffffff0002", name: "Code Review", model: "claude-sonnet" }),
  makeConvo({ id: "cccc3333-dddd-4eee-0fff-aaaaaaaa0003", name: "Debugging Session", model: "gpt-4o-mini" }),
  makeConvo({ id: "dddd4444-eeee-4fff-1aaa-bbbbbbbb0004", name: "Architecture Discussion", model: "claude-opus" }),
  makeConvo({ id: "eeee5555-ffff-4aaa-2bbb-cccccccc0005", name: "Quick Question", model: "gpt-4o" }),
];

describe("filterConversations", () => {
  it("returns all conversations when query is empty", () => {
    const result = filterConversations(fixtures, "");
    expect(result).toHaveLength(fixtures.length);
    expect(result.map((r) => r.conversation.id)).toEqual(
      fixtures.map((c) => c.id),
    );
  });

  it("matches conversation name (case-insensitive substring)", () => {
    const result = filterConversations(fixtures, "project");
    expect(result).toHaveLength(1);
    expect(result[0].conversation.name).toBe("Project Planning");
  });

  it("matches conversation name with mixed case query", () => {
    const result = filterConversations(fixtures, "PlAnNiNg");
    expect(result).toHaveLength(1);
    expect(result[0].conversation.name).toBe("Project Planning");
  });

  it("matches model name (case-insensitive substring)", () => {
    const result = filterConversations(fixtures, "claude");
    expect(result).toHaveLength(2);
    expect(result.map((r) => r.conversation.model)).toEqual([
      "claude-sonnet",
      "claude-opus",
    ]);
  });

  it("matches model with partial substring", () => {
    const result = filterConversations(fixtures, "opus");
    expect(result).toHaveLength(1);
    expect(result[0].conversation.model).toBe("claude-opus");
  });

  it("matches conversation id by prefix", () => {
    const result = filterConversations(fixtures, "cccc3333");
    expect(result).toHaveLength(1);
    expect(result[0].conversation.id).toBe(
      "cccc3333-dddd-4eee-0fff-aaaaaaaa0003",
    );
  });

  it("does NOT match conversation id by mid-string substring", () => {
    // "4eee" appears in the middle of the cccc3333 id, but should NOT match
    // because id matching is prefix-only.
    const result = filterConversations(fixtures, "4eee");
    // "4eee" is NOT a prefix of any id in our fixtures, so no matches.
    // (The id "cccc3333-dddd-4eee-..." has "4eee" in the middle, not at the start.)
    expect(result).toHaveLength(0);
  });

  it("ordinal stability: filtered rows carry their original index", () => {
    // "gpt-4o" matches fixtures[0], [2] (gpt-4o-mini contains "gpt-4o"), and [4]
    const result = filterConversations(fixtures, "gpt-4o");
    expect(result).toHaveLength(3);
    expect(result[0].originalIndex).toBe(0);
    expect(result[1].originalIndex).toBe(2);
    expect(result[2].originalIndex).toBe(4);
    // Ordinals displayed would be 1, 3, and 5 — not 1, 2, 3.
  });

  it("ordinal stability: filtering a single result keeps original position", () => {
    const result = filterConversations(fixtures, "Quick Question");
    expect(result).toHaveLength(1);
    expect(result[0].originalIndex).toBe(4);
    // The row should display as "5", not "1".
  });

  it("ordinal stability: all conversations have correct original indices", () => {
    const result = filterConversations(fixtures, "");
    for (let i = 0; i < result.length; i++) {
      expect(result[i].originalIndex).toBe(i);
    }
  });

  it("matches name OR model (union, not intersection)", () => {
    const result = filterConversations(fixtures, "review");
    // "review" matches "Code Review" (name) but not any model.
    expect(result).toHaveLength(1);
    expect(result[0].conversation.name).toBe("Code Review");
  });

  it("returns empty array when no conversations match", () => {
    const result = filterConversations(fixtures, "zzz-nonexistent");
    expect(result).toHaveLength(0);
  });

  it("works with an empty conversation list", () => {
    const result = filterConversations([], "anything");
    expect(result).toHaveLength(0);
  });

  it("empty query on empty list returns empty", () => {
    const result = filterConversations([], "");
    expect(result).toHaveLength(0);
  });

  it("id prefix match is case-insensitive", () => {
    const result = filterConversations(fixtures, "AAAA1111");
    expect(result).toHaveLength(1);
    expect(result[0].conversation.id).toBe(
      "aaaa1111-bbbb-4ccc-8ddd-eeeeeeee0001",
    );
  });

  it("multiple conversations can match the same query", () => {
    // "gpt" matches both gpt-4o and gpt-4o-mini models
    const result = filterConversations(fixtures, "gpt");
    expect(result).toHaveLength(3);
    expect(result.map((r) => r.conversation.model)).toEqual([
      "gpt-4o",
      "gpt-4o-mini",
      "gpt-4o",
    ]);
  });
});
