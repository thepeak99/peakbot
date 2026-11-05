// T10 (RED): the "Connect a phone or another PeakBot" panel
// (remote-profiles design §8.4, MP §7 item 3, amended by §8.8 MP task 3:
// panel rename + profile dropdown + Copy).
//
// The `./connectPhone` module does not exist yet — this file must fail
// with a missing-module import error.
//
// ── CONTRACT pinned here ─────────────────────────────────────────────────
//   export type Pairing =
//     | { available: true; links: string[] }      // the GET /api/pairing
//     | { available: false; reason: "loopback_bind" | "no_tls" };
//                                                 //   `Available.links` /
//                                                 //   `Unavailable.reason`
//   export function ConnectPhonePane({ pairing, profiles }: {
//     pairing: Pairing;
//     profiles: string[];   // the server's GLOBAL LOCAL profiles — the
//                           // dropdown's universe (§8.4)
//   })
//
// * The panel title is "Connect a phone or another PeakBot" (§8.4).
// * Available: one row per link (`connect-link-<i>`, host labels only).
//   The pairing link is HIDDEN until the user clicks Show QR
//   (`connect-show-qr`): the token-bearing URL is not in the DOM before
//   the click; after it, `connect-link` shows the current link and
//   `connect-qr` the QR (§7 MP: "The QR is hidden until the user clicks
//   Show QR (the token is on screen)").
// * The profile dropdown (`connect-profile`, one option per local
//   profile, first selected by default) appends `&profile=<name>`
//   CLIENT-side (§8.4) — the server's link never carries it.
// * "Copy link" (`connect-copy`) copies the current link (base link +
//   `&profile=<selected>`) to the clipboard.
// * Unavailable: the locked reason texts (MP §7):
//   loopback_bind → "Phones can't reach a loopback address — set
//     general.web.bind to 0.0.0.0:7823."
//   no_tls        → "Turn on general.web.tls first — the mobile app
//     connects over HTTPS only."
//   No Show-QR / Copy controls when unavailable.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ConnectPhonePane } from "./connectPhone";

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
  act(() => root.render(<div>{node}</div>));
}

function unmount() {
  act(() => root.unmount());
  host.remove();
}

afterEach(() => {
  unmount();
  vi.restoreAllMocks();
});

const CA = `sha256:${"0123456789abcdef".repeat(4)}`;
const LINK_1 = `https://10.2.0.48:7823/?token=pito&ca=${CA}`;
const LINK_2 = `https://192.168.7.7/?token=pito&ca=${CA}`;
const PROFILES = ["dev", "alt"];

const LOOPBACK_TEXT =
  "Phones can't reach a loopback address — set general.web.bind to 0.0.0.0:7823.";
const NO_TLS_TEXT =
  "Turn on general.web.tls first — the mobile app connects over HTTPS only.";

function q<T extends Element = HTMLElement>(sel: string): T {
  const el = host.querySelector(sel);
  if (!el) throw new Error(`missing element: ${sel}`);
  return el as T;
}

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function selectValue(el: HTMLSelectElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    Object.getPrototypeOf(el),
    "value",
  )!.set!;
  act(() => {
    setter.call(el, value);
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

describe("Connect a phone or another PeakBot (T10, §8.4)", () => {
  it("shows_the_title_and_one_row_per_link", () => {
    mount(
      <ConnectPhonePane pairing={{ available: true, links: [LINK_1, LINK_2] }} profiles={PROFILES} />,
    );
    expect(host.textContent).toContain("Connect a phone or another PeakBot");
    expect(q("[data-testid='connect-link-0']")).toBeDefined();
    expect(q("[data-testid='connect-link-1']")).toBeDefined();
    expect(host.querySelector("[data-testid='connect-link-2']")).toBeNull();
  });

  it("the_link_is_hidden_until_show_qr_is_clicked", () => {
    mount(<ConnectPhonePane pairing={{ available: true, links: [LINK_1] }} profiles={PROFILES} />);

    // Before the click: the token-bearing URL is nowhere in the DOM.
    expect(host.textContent).not.toContain("token=pito");
    expect(host.querySelector("[data-testid='connect-link']")).toBeNull();

    // After the click: the link (first profile selected by default) and
    // the QR are on screen.
    click(q("[data-testid='connect-show-qr']"));
    const link = q("[data-testid='connect-link']");
    expect(link.textContent).toContain("token=pito");
    expect(link.textContent).toBe(`${LINK_1}&profile=dev`);
    expect(q("[data-testid='connect-qr']"), "the QR renders").toBeDefined();
  });

  it("the_profile_dropdown_appends_profile_client_side", () => {
    mount(<ConnectPhonePane pairing={{ available: true, links: [LINK_1] }} profiles={PROFILES} />);
    click(q("[data-testid='connect-show-qr']"));

    const sel = q<HTMLSelectElement>("[data-testid='connect-profile']");
    expect([...sel.options].map((o) => o.value)).toEqual(PROFILES);
    selectValue(sel, "alt");
    expect(q("[data-testid='connect-link']").textContent).toBe(`${LINK_1}&profile=alt`);
  });

  it("copy_link_copies_the_link_with_the_selected_profile", () => {
    const writeText = vi.fn();
    Object.defineProperty(window.navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
    mount(<ConnectPhonePane pairing={{ available: true, links: [LINK_1] }} profiles={PROFILES} />);
    click(q("[data-testid='connect-show-qr']"));
    selectValue(q<HTMLSelectElement>("[data-testid='connect-profile']"), "alt");
    click(q("[data-testid='connect-copy']"));

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith(`${LINK_1}&profile=alt`);
  });

  it("unavailable_states_show_the_locked_reason_text", () => {
    mount(
      <ConnectPhonePane pairing={{ available: false, reason: "loopback_bind" }} profiles={PROFILES} />,
    );
    expect(host.textContent).toContain(LOOPBACK_TEXT);
    expect(host.querySelector("[data-testid='connect-show-qr']")).toBeNull();
    expect(host.querySelector("[data-testid='connect-copy']")).toBeNull();

    unmount();
    mount(
      <ConnectPhonePane pairing={{ available: false, reason: "no_tls" }} profiles={PROFILES} />,
    );
    expect(host.textContent).toContain(NO_TLS_TEXT);
    expect(host.textContent).not.toContain("token=pito");
  });
});
