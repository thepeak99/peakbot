import { describe, it, expect } from "vitest";
import { isPinnedAt } from "./useTranscriptScroll";

// The pin predicate decides whether new messages are allowed to drag the
// viewport. Threshold is 80px from the bottom.
describe("isPinnedAt", () => {
  it("is pinned at the exact bottom", () => {
    expect(
      isPinnedAt({ scrollTop: 900, scrollHeight: 1500, clientHeight: 600 }),
    ).toBe(true);
  });

  it("is pinned just inside the threshold and unpinned just outside", () => {
    expect(
      isPinnedAt({ scrollTop: 821, scrollHeight: 1500, clientHeight: 600 }),
    ).toBe(true);
    expect(
      isPinnedAt({ scrollTop: 820, scrollHeight: 1500, clientHeight: 600 }),
    ).toBe(false);
  });

  it("is unpinned while reading history", () => {
    expect(
      isPinnedAt({ scrollTop: 0, scrollHeight: 1500, clientHeight: 600 }),
    ).toBe(false);
  });

  it("is pinned when the content does not overflow", () => {
    expect(
      isPinnedAt({ scrollTop: 0, scrollHeight: 600, clientHeight: 600 }),
    ).toBe(true);
  });
});
