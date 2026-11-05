// T10 (RED): Settings support for remote profiles (remote-profiles design
// §9 T10, §8.4).
//
// Written against the contract below. `./profiles` (ProfilesPane +
// ProfilePage) exists, so the tests compile; they are RED because the
// remote-aware testids and the "+ Add remote profile" flow do not exist
// yet.
//
// ── CONTRACT pinned here ─────────────────────────────────────────────────
// LIST (ProfilesPane):
//   * a profile entry with a `remote:` block renders a
//     `<Badge data-testid="profile-remote-badge">remote</Badge>` on its
//     card (local entries render no such badge);
//   * a configured `ui.color` renders a colour indicator
//     `<span data-testid="profile-color-<i>"><color name></span>` on the
//     card (absent when no colour is configured);
//   * a "+ Add remote profile" button (`data-testid="profile-add-remote"`)
//     next to "+ New profile" opens the paste step
//     (`data-testid="remote-add-paste"`): input `remote-add-input`,
//     apply button `remote-add-apply`, error slot `remote-add-error`.
//     Applying a valid pairing link creates ONE new entry named
//     `defaultRemoteName(profile, host)` (§8.4) with the def
//     `{ remote: { url, token, [ca], profile } }` (a `ca` key only when
//     the link carries one — no silent system trust) and opens it via
//     `onOpenProfile(name)`. An invalid link shows the
//     `parsePairingLink` error in `remote-add-error` and edits nothing.
// PAGE (ProfilePage for a `remote:` entry) — the remote form REPLACES the
//   agents editor (no `profile-agents`, no Tools/MCP/Env/Memory/Knowledge
//   cards):
//   * `remote-form` wrapper with fields
//     `remote-url`, `remote-token` (a SecretInput: masked by default),
//     `remote-ca`, `remote-profile`, `remote-agent` (optional) and a
//     `remote-color` select over the 8 TabColor names;
//   * field edits write the draft segments
//     `["profiles", <name>, "remote", <field>]` and
//     `["profiles", <name>, "ui", "color"]`;
//   * an empty `ca` shows the §4.4 required hint in
//     `remote-ca-required` (the pin-or-system choice, verbatim);
//   * server validation errors (ctx.errors) whose path is
//     `["profiles", <name>, "remote", <field>]` render on that field:
//     `remote-<field>-error`.
//
// The saved draft keeps a `remote:` block with NO local fields (RP1).
// The token is masked in the DOM and preserved on save: the pane never
// writes the mask into the draft (§7: the server keeps the full text).

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ProfilePage, ProfilesPane } from "./profiles";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { get, parse, set, stringify } from "./yamlDoc";
import { ToastProvider } from "./Toast";
import type { ConfigErr, Scope, Seg, SettingsMeta } from "./types";

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

afterEach(unmount);

interface Edit {
  path: Seg[];
  value: unknown;
}

const TAB_NAMES = ["session", "todo", "files", "tasks", "bash", "agents", "settings"];

function meta(): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: "/tmp/peakbot/config.yaml",
    repo_path: "/tmp/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: [],
    tab_names: TAB_NAMES,
    skills: [],
    builtin_persona: "",
    mcp_status: [],
    boot: { bind: "0.0.0.0:7823", token_source: "config", tls: true },
    facts: {
      os: "linux",
      arch: "amd64",
      exe_path: "/usr/local/bin/peakbot",
      data_dir: "/tmp/peakbot-data",
      version: "0.25.0",
      install: null,
    },
  };
}

// ── fixture docs ─────────────────────────────────────────────────────────

const CA = `sha256:${"0123456789abcdef".repeat(4)}`;

/// §4.4 messages (locked by T3) — the form surfaces them verbatim.
const CA_MISSING_MESSAGE =
  "pin the server's CA (`ca: sha256:…` from its pairing link) or use `ca: system` for a publicly trusted certificate";
const URL_LOOPBACK_MESSAGE =
  "remote profiles connect over HTTPS — http is allowed only for 127.0.0.1 / localhost";

/// The §8.4 pairing link (server entry link + &ca, + the dropdown's &profile).
const FULL_LINK = `https://10.2.0.48:7823/?token=pito&ca=${CA}&profile=dev`;
const LINK_WITHOUT_CA = "https://10.2.0.48:7823/?token=pito&profile=dev";
const NOT_A_LINK = "Not a pairing link — expected https://host:port/?token=…";

const LIST_DOC = `
profiles:
  dev:
    agents:
      main:
        model: sonnet
  home-dev:
    remote:
      url: https://10.2.0.48:7823
      token: pito
      ca: ${CA}
      profile: dev
    ui:
      color: purple
`;

const PAGE_DOC = `
profiles:
  dev:
    agents:
      main:
        model: sonnet
  home-dev:
    remote:
      url: https://10.2.0.48:7823
      token: pito
      ca: ${CA}
      profile: dev
    ui:
      color: purple
`;

const NO_CA_DOC = `
profiles:
  home-dev:
    remote:
      url: https://10.2.0.48:7823
      token: pito
      profile: dev
`;

function ctxFor(
  yaml: string,
  edits: Edit[],
  opts: { scope?: Scope; errors?: ConfigErr[] } = {},
): SettingsCtxValue {
  return {
    scope: opts.scope ?? "global",
    meta: meta(),
    doc: parse(yaml),
    globalDoc: parse(yaml),
    edit: (path, value) => {
      edits.push({ path, value });
    },
    errors: opts.errors ?? [],
    switchToGlobal: () => {},
  };
}

function mountList(yaml: string, onOpenProfile?: (name: string) => void): Edit[] {
  const edits: Edit[] = [];
  mount(
    <ToastProvider>
      <SettingsCtx.Provider value={ctxFor(yaml, edits)}>
        <ProfilesPane onOpenProfile={onOpenProfile} />
      </SettingsCtx.Provider>
    </ToastProvider>,
  );
  return edits;
}

interface PageOpts {
  errors?: ConfigErr[];
  scope?: Scope;
}

function mountPage(yaml: string, profile: string, opts: PageOpts = {}): Edit[] {
  const edits: Edit[] = [];
  mount(
    <ToastProvider>
      <SettingsCtx.Provider value={ctxFor(yaml, edits, opts)}>
        <ProfilePage profile={profile} />
      </SettingsCtx.Provider>
    </ToastProvider>,
  );
  return edits;
}

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

/** Set a controlled input/select value (React's native-setter dance). */
function setValue(el: HTMLInputElement | HTMLSelectElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    Object.getPrototypeOf(el),
    "value",
  )!.set!;
  act(() => {
    setter.call(el, value);
    el.dispatchEvent(
      el instanceof HTMLSelectElement
        ? new Event("change", { bubbles: true })
        : new Event("input", { bubbles: true }),
    );
  });
}

function eqPath(a: Seg[], b: Seg[]): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

function lastWrite(edits: Edit[], path: Seg[]): Edit | undefined {
  for (let i = edits.length - 1; i >= 0; i--) {
    if (eqPath(edits[i].path, path)) return edits[i];
  }
  return undefined;
}

// ── list ─────────────────────────────────────────────────────────────────

describe("Profiles list: remote entries (T10)", () => {
  it("marks_remote_profiles_with_a_badge_and_the_configured_colour", () => {
    mountList(LIST_DOC);
    const local = q("[data-testid='profile-card-0']");
    const remote = q("[data-testid='profile-card-1']");
    expect(local.textContent).toContain("dev");
    expect(remote.textContent).toContain("home-dev");

    // The remote card carries the remote badge; the local one does not.
    expect(remote.querySelector("[data-testid='profile-remote-badge']")).not.toBeNull();
    expect(
      remote.querySelector("[data-testid='profile-remote-badge']")!.textContent,
    ).toBe("remote");
    expect(
      local.querySelector("[data-testid='profile-remote-badge']"),
      "local entries have no remote badge",
    ).toBeNull();

    // The configured ui.color is shown on the remote card.
    const colour = remote.querySelector("[data-testid='profile-color-1']");
    expect(colour, "the remote card shows its colour").not.toBeNull();
    expect(colour!.textContent).toBe("purple");
  });
});

// ── page: the remote form ────────────────────────────────────────────────

describe("Profile page: remote form (T10)", () => {
  it("opening_a_remote_profile_shows_the_remote_form_not_the_agents_editor", () => {
    mountPage(PAGE_DOC, "home-dev");
    expect(q("[data-testid='remote-form']"), "the remote form renders").toBeDefined();

    // Fields, prefilled from the draft.
    expect(q<HTMLInputElement>("[data-testid='remote-url'] input").value).toBe(
      "https://10.2.0.48:7823",
    );
    expect(q<HTMLInputElement>("[data-testid='remote-ca'] input").value).toBe(CA);
    expect(q<HTMLInputElement>("[data-testid='remote-profile'] input").value).toBe("dev");
    expect(q<HTMLInputElement>("[data-testid='remote-agent'] input").value).toBe("");
    const colour = q<HTMLSelectElement>("[data-testid='remote-color']");
    expect(colour.value).toBe("purple");

    // The agents editor and the local-only cards are gone.
    expect(host.querySelector("[data-testid='profile-agents']"), "no agents editor").toBeNull();
    expect(host.querySelector("[data-testid='profile-tools-card']"), "no tools card").toBeNull();
    expect(host.querySelector("[data-testid='profile-mcps-card']"), "no mcps card").toBeNull();
  });

  it("remote_token_is_masked_and_preserved_on_save", () => {
    const edits = mountPage(PAGE_DOC, "home-dev");

    // Masked by default: the token is NOT in the DOM.
    expect(host.textContent).not.toContain("pito");
    expect(q("[data-testid='remote-token'] [data-testid='value']").textContent).toBe(
      "••••••••",
    );
    expect(
      edits.filter((e) => eqPath(e.path, ["profiles", "home-dev", "remote", "token"])),
      "rendering writes nothing to the token",
    ).toHaveLength(0);

    // Edit one other field, apply all edits to a fresh doc, stringify:
    // the token is preserved (the mask never reaches the draft).
    setValue(q<HTMLInputElement>("[data-testid='remote-url'] input"), "https://10.9.9.9:7823");
    const doc = parse(PAGE_DOC);
    for (const e of edits) set(doc, e.path, e.value);
    const text = stringify(doc);
    expect(text).toContain("token: pito");
    expect(text).toContain("https://10.9.9.9:7823");
  });

  it("remote_form_edits_write_the_remote_segments", () => {
    const edits = mountPage(PAGE_DOC, "home-dev");
    const base: Seg[] = ["profiles", "home-dev"];

    setValue(q<HTMLInputElement>("[data-testid='remote-url'] input"), "https://10.9.9.9:7823");
    let w = lastWrite(edits, [...base, "remote", "url"]);
    expect(w, "url edit").toBeDefined();
    expect(w!.value).toBe("https://10.9.9.9:7823");

    setValue(q<HTMLInputElement>("[data-testid='remote-ca'] input"), "system");
    w = lastWrite(edits, [...base, "remote", "ca"]);
    expect(w, "ca edit").toBeDefined();
    expect(w!.value).toBe("system");

    setValue(q<HTMLInputElement>("[data-testid='remote-profile'] input"), "ops");
    w = lastWrite(edits, [...base, "remote", "profile"]);
    expect(w, "profile edit").toBeDefined();
    expect(w!.value).toBe("ops");

    setValue(q<HTMLInputElement>("[data-testid='remote-agent'] input"), "team");
    w = lastWrite(edits, [...base, "remote", "agent"]);
    expect(w, "agent edit").toBeDefined();
    expect(w!.value).toBe("team");

    setValue(q<HTMLSelectElement>("[data-testid='remote-color']"), "teal");
    w = lastWrite(edits, [...base, "ui", "color"]);
    expect(w, "colour edit").toBeDefined();
    expect(w!.value).toBe("teal");
  });

  it("saving_emits_a_remote_block_with_no_local_fields", () => {
    const edits = mountPage(PAGE_DOC, "home-dev");
    setValue(q<HTMLInputElement>("[data-testid='remote-url'] input"), "https://10.9.9.9:7823");
    setValue(q<HTMLInputElement>("[data-testid='remote-profile'] input"), "ops");

    // Apply every recorded edit to a fresh doc — the entry the save would
    // send is a `remote:` block (plus optional ui) and nothing local.
    const doc = parse(PAGE_DOC);
    for (const e of edits) set(doc, e.path, e.value);
    const def = get(doc, ["profiles", "home-dev"]) as Record<string, unknown>;
    expect(Object.keys(def).sort()).toEqual(["remote", "ui"]);
    const remote = def.remote as Record<string, unknown>;
    expect(remote.url).toBe("https://10.9.9.9:7823");
    expect(remote.profile).toBe("ops");
    expect(remote.token).toBe("pito"); // untouched, preserved
    expect(remote.ca).toBe(CA);
    expect("agents" in def).toBe(false);
    expect("tools" in def).toBe(false);
    expect(stringify(doc)).toContain("remote:");
  });

  it("an_empty_ca_is_required_with_the_doc_message", () => {
    mountPage(NO_CA_DOC, "home-dev");
    expect(q<HTMLInputElement>("[data-testid='remote-ca'] input").value).toBe("");
    const hint = q("[data-testid='remote-ca-required']");
    expect(hint, "an empty ca shows the required hint").toBeDefined();
    expect(hint.textContent).toBe(CA_MISSING_MESSAGE);
  });

  it("server_validation_errors_appear_on_the_right_field", () => {
    // The §4.4 ca error at `profiles.home-dev.remote.ca` → on the ca field.
    mountPage(PAGE_DOC, "home-dev", {
      errors: [
        {
          file: "global",
          path: ["profiles", "home-dev", "remote", "ca"],
          message: CA_MISSING_MESSAGE,
        },
      ],
    });
    const caErr = q("[data-testid='remote-ca-error']");
    expect(caErr.textContent).toContain(CA_MISSING_MESSAGE);

    // The §4.4 url error → on the url field (and NOT on the ca field).
    unmount();
    mountPage(PAGE_DOC, "home-dev", {
      errors: [
        {
          file: "global",
          path: ["profiles", "home-dev", "remote", "url"],
          message: URL_LOOPBACK_MESSAGE,
        },
      ],
    });
    const urlErr = q("[data-testid='remote-url-error']");
    expect(urlErr.textContent).toContain(URL_LOOPBACK_MESSAGE);
    expect(
      host.querySelector("[data-testid='remote-ca-error']"),
      "the url error must not leak onto the ca field",
    ).toBeNull();
  });
});

// ── add remote profile: paste a pairing link ─────────────────────────────

describe("Add remote profile: paste a pairing link (T10, §8.4)", () => {
  function addStep(yaml: string, onOpenProfile?: (name: string) => void): Edit[] {
    const edits = mountList(yaml, onOpenProfile);
    click(q("[data-testid='profile-add-remote']"));
    expect(
      q("[data-testid='remote-add-paste']"),
      "the paste step appears",
    ).toBeDefined();
    return edits;
  }

  it("paste_a_valid_link_creates_the_entry_with_all_fields_and_opens_it", () => {
    let opened: string | null = null;
    const edits = addStep(LIST_DOC, (n) => {
      opened = n;
    });

    setValue(q<HTMLInputElement>("[data-testid='remote-add-input']"), FULL_LINK);
    click(q("[data-testid='remote-add-apply']"));

    // The entry is named `<profile> on <host>` (§8.4) and opened.
    expect(opened).toBe("dev on 10.2.0.48");
    const w = lastWrite(edits, ["profiles"]);
    expect(w, "the profiles map is rewritten").toBeDefined();
    const map = w!.value as Record<string, unknown>;
    // The existing entries are kept, the new one is added.
    expect(Object.keys(map)).toEqual(["dev", "home-dev", "dev on 10.2.0.48"]);
    const def = map["dev on 10.2.0.48"] as Record<string, unknown>;
    const remote = def.remote as Record<string, unknown>;
    expect(remote.url).toBe("https://10.2.0.48:7823");
    expect(remote.token).toBe("pito");
    expect(remote.ca).toBe(CA);
    expect(remote.profile).toBe("dev");
    expect("agents" in def).toBe(false); // no local fields (RP1)
  });

  it("paste_a_link_without_ca_creates_the_entry_without_a_ca_key", () => {
    let opened: string | null = null;
    const edits = addStep(LIST_DOC, (n) => {
      opened = n;
    });

    setValue(q<HTMLInputElement>("[data-testid='remote-add-input']"), LINK_WITHOUT_CA);
    click(q("[data-testid='remote-add-apply']"));

    expect(opened).toBe("dev on 10.2.0.48");
    const w = lastWrite(edits, ["profiles"]);
    const map = w!.value as Record<string, unknown>;
    const def = map["dev on 10.2.0.48"] as Record<string, unknown>;
    const remote = def.remote as Record<string, unknown>;
    expect(remote.ca, "no silent system trust: ca stays absent").toBeUndefined();
    expect(remote.url).toBe("https://10.2.0.48:7823");
    expect(remote.token).toBe("pito");
    expect(remote.profile).toBe("dev");
  });

  it("paste_an_invalid_link_shows_the_error_and_edits_nothing", () => {
    const edits = addStep(LIST_DOC);

    setValue(q<HTMLInputElement>("[data-testid='remote-add-input']"), "hello");
    click(q("[data-testid='remote-add-apply']"));
    expect(q("[data-testid='remote-add-error']").textContent).toBe(NOT_A_LINK);
    expect(lastWrite(edits, ["profiles"]), "no edit for an invalid link").toBeUndefined();

    // A link without a token param is not a pairing link either.
    setValue(
      q<HTMLInputElement>("[data-testid='remote-add-input']"),
      `https://10.2.0.48:7823/?ca=${CA}`,
    );
    click(q("[data-testid='remote-add-apply']"));
    expect(q("[data-testid='remote-add-error']").textContent).toBe(NOT_A_LINK);
    expect(lastWrite(edits, ["profiles"]), "still no edit").toBeUndefined();
  });
});