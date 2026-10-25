// T34b regression (QA fix 3): the error panel title must NOT say
// "Save failed" before any Save attempt — that is a SAVE outcome. Live
// validation errors show the neutral `N problem(s)`; only after a failed
// Save (the shell's saveFailed flag, set on a 422 PUT) does the title keep
// "Save failed — N problem(s)".

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ErrorPanel } from "./ErrorPanel";
import type { ConfigErr } from "./types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root;
let host: HTMLElement;

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root.render(node));
}

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const ERRS: ConfigErr[] = [
  { file: "global", path: ["providers", 0, "api_key"], message: "required" },
  { file: "global", path: ["profiles", "default", "agents", "main", "model"], message: "unknown alias" },
];

describe("error panel title (T34b)", () => {
  it("live_validation_errors_before_a_save_are_neutral", () => {
    mount(<ErrorPanel errors={ERRS} scope="global" saveFailed={false} onFix={() => {}} />);
    const title = host.querySelector('[data-testid="error-panel-title"]')!;
    expect(title.textContent).toBe("⚠ 2 problems");
    expect(title.textContent).not.toContain("Save failed");
  });

  it("after_a_failed_save_the_title_says_save_failed", () => {
    mount(<ErrorPanel errors={ERRS} scope="global" saveFailed={true} onFix={() => {}} />);
    const title = host.querySelector('[data-testid="error-panel-title"]')!;
    expect(title.textContent).toBe("✕ Save failed — 2 problems");
  });

  it("singular_problem_wording", () => {
    mount(<ErrorPanel errors={[ERRS[0]]} scope="global" saveFailed={false} onFix={() => {}} />);
    expect(host.querySelector('[data-testid="error-panel-title"]')!.textContent).toBe(
      "⚠ 1 problem",
    );
    mount(<ErrorPanel errors={[ERRS[0]]} scope="global" saveFailed={true} onFix={() => {}} />);
    expect(host.querySelector('[data-testid="error-panel-title"]')!.textContent).toBe(
      "✕ Save failed — 1 problem",
    );
  });
});
