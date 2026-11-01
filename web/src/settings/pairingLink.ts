// T10 pairing-link split (design §8.4). The URL is split into the form
// fields `url` (origin), `token`, `ca`, `profile`; the split NEVER judges
// beyond "is this a pairing link at all" — `Config::parse` is the validator.

export interface PairingFields {
  /** The ORIGIN only: scheme://host[:port] (the default port is dropped). */
  url: string;
  /** The `?token=` value (required). */
  token: string;
  /** The `ca=` value, or null when absent (the form then requires a choice). */
  ca: string | null;
  /** The `profile=` value, or null when absent. */
  profile: string | null;
}

/// The locked error for a text that is not a pairing link at all.
const NOT_A_LINK = "Not a pairing link — expected https://host:port/?token=…";

export function parsePairingLink(text: string): PairingFields {
  let u: URL;
  try {
    u = new URL(text);
  } catch {
    throw new Error(NOT_A_LINK);
  }
  // https only: a pairing link carries a CA pin, and plaintext is
  // expressible in config only for loopback (which has no pairing link).
  if (u.protocol !== "https:") throw new Error(NOT_A_LINK);
  const token = u.searchParams.get("token");
  if (token === null) throw new Error(NOT_A_LINK);
  return {
    url: u.origin,
    token,
    ca: u.searchParams.get("ca"),
    profile: u.searchParams.get("profile"),
  };
}

/**
 * `<profile> on <host>` (or just `<host>` without a profile), sanitised to
 * the config name rule `[A-Za-z0-9_ .-]{1,64}`: disallowed chars removed,
 * trimmed, capped at 64.
 */
export function defaultRemoteName(profile: string | null, host: string): string {
  const raw = profile === null ? host : `${profile} on ${host}`;
  return raw.replace(/[^A-Za-z0-9_ .-]/g, "").trim().slice(0, 64);
}
