// T10 "Connect a phone or another PeakBot" panel (design §8.4, amending
// MP §4.4). The token-bearing link is NOT in the DOM until the user clicks
// Show QR; `&profile=<name>` is appended CLIENT-side — the server's links
// never carry it.

import { useState } from "react";

export type Pairing =
  | { available: true; links: string[] }
  | { available: false; reason: "loopback_bind" | "no_tls" };

/** Decode the external-tagged `GET /api/pairing` JSON (design §8.4). */
export function decodePairing(json: unknown): Pairing {
  const o = (json ?? {}) as Record<string, unknown>;
  const links = (o.Available as { links?: unknown } | undefined)?.links;
  if (Array.isArray(links)) {
    return { available: true, links: links.filter((l): l is string => typeof l === "string") };
  }
  const reason = (o.Unavailable as { reason?: unknown } | undefined)?.reason;
  if (reason === "loopback_bind" || reason === "no_tls") return { available: false, reason };
  // An unknown shape offers no links — report it as "no TLS" (the weaker,
  // still-actionable reason) rather than crash the pane.
  return { available: false, reason: "no_tls" };
}

/// The locked unavailable reason texts (MP §7).
const REASON_TEXT = {
  loopback_bind: "Phones can't reach a loopback address — set general.web.bind to 0.0.0.0:7823.",
  no_tls: "Turn on general.web.tls first — the mobile app connects over HTTPS only.",
} as const;

const CARD = "rounded-lg border border-zinc-800 bg-zinc-900/40 p-3";
const GHOST_BTN =
  "cursor-pointer rounded border border-zinc-700 px-2 py-0.5 text-[11px] text-zinc-300 hover:bg-zinc-800";

/** A row's label: the origin only — the token URL never shows in the list. */
function hostLabel(link: string): string {
  try {
    return new URL(link).origin;
  } catch {
    return "(unparseable link)";
  }
}

export interface ConnectPhonePaneProps {
  pairing: Pairing;
  /** The server's GLOBAL LOCAL profiles — the dropdown's universe (§8.4). */
  profiles: string[];
}

export function ConnectPhonePane({ pairing, profiles }: ConnectPhonePaneProps) {
  const [shown, setShown] = useState(false);
  const [selected, setSelected] = useState("");
  // First profile by default; re-derived so a late-arriving list (repo
  // scope loads the global draft on demand) selects the first entry.
  const sel = selected !== "" ? selected : profiles[0] ?? "";

  if (!pairing.available) {
    return (
      <div className={CARD}>
        <h3 className="text-sm font-semibold">Connect a phone or another PeakBot</h3>
        <p data-testid="connect-unavailable" className="mt-2 text-xs text-zinc-400">
          {REASON_TEXT[pairing.reason]}
        </p>
      </div>
    );
  }

  const base = pairing.links[0] ?? "";
  const link = `${base}&profile=${sel}`;

  return (
    <div className={CARD}>
      <h3 className="text-sm font-semibold">Connect a phone or another PeakBot</h3>
      <p className="mt-1 text-xs text-zinc-500">
        Open a link on a phone (or paste it in Settings → Profiles on the other PeakBot).
      </p>
      <ul className="mt-2 space-y-1">
        {pairing.links.map((l, i) => (
          <li key={i} data-testid={`connect-link-${i}`} className="font-mono text-xs text-zinc-300">
            {hostLabel(l)}
          </li>
        ))}
      </ul>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <select
          data-testid="connect-profile"
          value={sel}
          onChange={(e) => setSelected(e.target.value)}
          className="cursor-pointer rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 text-xs text-zinc-100 focus:border-zinc-500 focus:outline-none"
        >
          {profiles.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
        {shown ? (
          <button
            data-testid="connect-copy"
            type="button"
            onClick={() => {
              navigator.clipboard?.writeText(link);
            }}
            className={GHOST_BTN}
          >
            Copy link
          </button>
        ) : (
          <button data-testid="connect-show-qr" type="button" onClick={() => setShown(true)} className={GHOST_BTN}>
            Show QR
          </button>
        )}
      </div>
      {shown && (
        <div className="mt-3 space-y-2">
          <code
            data-testid="connect-link"
            className="block break-all rounded border border-zinc-800 bg-zinc-950 px-2 py-1.5 font-mono text-[11px] text-zinc-300"
          >
            {link}
          </code>
          {/* No QR dependency is in package.json — a monospace placeholder
              of the same payload until one is added (follow-up). */}
          <div
            data-testid="connect-qr"
            className="max-w-md rounded border border-zinc-800 bg-zinc-950 p-2 font-mono text-[10px] text-zinc-400"
          >
            {link}
          </div>
        </div>
      )}
    </div>
  );
}
