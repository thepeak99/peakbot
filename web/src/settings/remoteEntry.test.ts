// The shared pairing-link → remote-profile-entry seam, extracted from the
// Settings → Profiles paste flow so the paste box and the first-run "Connect
// to my PeakBot server" path produce the SAME config entry (one validator).
//
// RED today: `remoteEntryFromLink` does not exist in ./pairingLink — this
// file fails on the missing export. Everything else (parsePairingLink,
// defaultRemoteName) already holds.
//
// ── CONTRACT pinned here ─────────────────────────────────────────────────
//   export interface RemoteEntry {
//     /** The profile map name: `<profile> on <host>` sanitised to the name
//      *  rule (defaultRemoteName). The host label is the URL's HOSTNAME —
//      *  the existing paste flow and its tests pin it port-less
//      *  (`dev on 10.2.0.48` from a `:7823` link). */
//     name: string;
//     /** The profiles-map value: a `remote:` block and NOTHING local (RP1).
//      *  `url` is the ORIGIN (scheme://host[:port] — the port stays here,
//      *  only the name drops it); `ca` is present iff the link carried one
//      *  (no silent system trust); `profile` is the link's `profile=` or
//      *  "" when absent (REQUIRED on the remote, the form asks for it). */
//     entry: { remote: { url: string; token: string; ca?: string; profile: string } };
//   }
//   export function remoteEntryFromLink(text: string): RemoteEntry
//     — throws an Error with the locked "Not a pairing link — expected
//       https://host:port/?token=…" message for any text that is not a
//       pairing link (the split never judges beyond that; Config::parse is
//       the validator).
//
// Links now carry the server's port:
// `https://10.2.0.48:7899/?token=…&ca=sha256:…&profile=dev`.

import { describe, expect, it } from "vitest";
import { remoteEntryFromLink } from "./pairingLink";

const CA = `sha256:${"0123456789abcdef".repeat(4)}`;

/// A pairing link on a NON-default port (the fixed /api/pairing shape).
const LINK_WITH_PORT = `https://10.2.0.48:7899/?token=pito&ca=${CA}&profile=dev`;

describe("remoteEntryFromLink (shared paste/wizard seam)", () => {
  it("derives_name_and_entry_from_a_ported_link", () => {
    const { name, entry } = remoteEntryFromLink(LINK_WITH_PORT);

    // Name: `<profile> on <host>`, host label port-less (the paste flow's
    // existing tests pin `dev on 10.2.0.48` for a ported link).
    expect(name).toBe("dev on 10.2.0.48");

    // Entry: a remote: block whose url KEEPS the port.
    const remote = entry.remote;
    expect(remote.url).toBe("https://10.2.0.48:7899");
    expect(remote.token).toBe("pito");
    expect(remote.ca).toBe(CA);
    expect(remote.profile).toBe("dev");
  });

  it("a_link_without_ca_yields_no_ca_key", () => {
    const { entry } = remoteEntryFromLink("https://10.2.0.48:7899/?token=pito&profile=dev");
    expect("ca" in entry.remote).toBe(false); // no silent system trust
    expect(entry.remote.url).toBe("https://10.2.0.48:7899");
    expect(entry.remote.profile).toBe("dev");
  });

  it("a_link_without_profile_names_by_host_and_leaves_profile_blank", () => {
    const { name, entry } = remoteEntryFromLink(`https://bot.example.ts.net/?token=abc123&ca=${CA}`);
    expect(name).toBe("bot.example.ts.net");
    expect(entry.remote.profile).toBe(""); // the form requires the choice
  });

  it("the_entry_carries_no_local_fields", () => {
    const { entry } = remoteEntryFromLink(LINK_WITH_PORT);
    // A `remote:` block (plus nothing else) — the agent/tools live on the
    // remote server, so the saved entry must stay remote-only (RP1).
    expect(Object.keys(entry)).toEqual(["remote"]);
  });

  it("sanitises_the_name_to_the_config_name_rule", () => {
    // Disallowed chars removed, trimmed, 64-char cap (defaultRemoteName).
    const { name } = remoteEntryFromLink(
      `https://10.2.0.48:7899/?token=pito&ca=${CA}&profile=dev!!`,
    );
    expect(name).toBe("dev on 10.2.0.48");

    const { name: long } = remoteEntryFromLink(
      `https://h/?token=x&ca=${CA}&profile=${"a".repeat(80)}`,
    );
    expect(long.length).toBeLessThanOrEqual(64);
  });

  it("rejects_a_text_that_is_not_a_pairing_link", () => {
    const NOT_A_LINK = "Not a pairing link — expected https://host:port/?token=…";
    for (const bad of [
      "",
      "hello",
      "https://10.2.0.48:7899/",
      `https://10.2.0.48:7899/?ca=${CA}`,
      "http://127.0.0.1:7899/?token=pito",
    ]) {
      expect(() => remoteEntryFromLink(bad)).toThrow(NOT_A_LINK);
    }
  });
});
