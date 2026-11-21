// P4 — cloud wire types (design §1) + the ONE boundary parser.

export type CloudState =
  | { state: "disabled" }
  | { state: "logged_out"; reason: "never" | "expired" | "signed_out" }
  | { state: "connecting"; attempt: number; last_error: string | null }
  | {
      state: "pending";
      empty_account: boolean;
      sas: string[] | null;
      rejected: boolean;
    }
  | { state: "active"; devices: Device[]; requests: EnrollRequest[] }
  | {
      state: "trust";
      trust_error: "genesis_mismatch" | "fork" | "revoked" | "chain_invalid" | "seed_store_error";
    };

export type StateBody = CloudState & { last_login_error: string | null };

export type Device = {
  id: string;
  name: string;
  platform: string;
  online: boolean | null;
  slug: string;
  self: boolean;
};

export type EnrollRequest = {
  letter_id: string;
  device_id: string;
  name: string;
  platform: string;
  sas: string[];
  expires_at: number;
};

export type CloudError = { error: string; message: string };

type Rec = Record<string, unknown>;

const isRec = (v: unknown): v is Rec => typeof v === "object" && v !== null;
const isStr = (v: unknown): v is string => typeof v === "string";
const isSix = (v: unknown): v is string[] =>
  Array.isArray(v) && v.length === 6 && v.every(isStr);

const TRUST_ERRORS = ["genesis_mismatch", "fork", "revoked", "chain_invalid", "seed_store_error"];
const LOGOUT_REASONS = ["never", "expired", "signed_out"];

function isDevice(v: unknown): v is Device {
  return (
    isRec(v) &&
    isStr(v.id) &&
    isStr(v.name) &&
    isStr(v.platform) &&
    (v.online === null || typeof v.online === "boolean") &&
    isStr(v.slug) &&
    typeof v.self === "boolean"
  );
}

function isRequest(v: unknown): v is EnrollRequest {
  return (
    isRec(v) &&
    isStr(v.letter_id) &&
    isStr(v.device_id) &&
    isStr(v.name) &&
    isStr(v.platform) &&
    isSix(v.sas) &&
    typeof v.expires_at === "number"
  );
}

/** The ONE boundary parser: the only place a raw SSE/JSON payload is checked.
 *
 * Checks the `state` tag, `sas.length === 6`, and that a `pending` body
 * carries a `rejected` boolean (strict — the server always sends it).
 * Unknown tag / malformed → `null` ("no snapshot"). Never throws.
 */
export function parseStateBody(json: unknown): StateBody | null {
  if (!isRec(json)) return null;
  const lle = json.last_login_error;
  if (lle !== null && !isStr(lle)) return null;
  const ok = (cs: CloudState): StateBody => ({ ...cs, last_login_error: lle });
  switch (json.state) {
    case "disabled":
      return ok({ state: "disabled" });
    case "logged_out":
      return isStr(json.reason) && LOGOUT_REASONS.includes(json.reason)
        ? ok({ state: "logged_out", reason: json.reason as "never" })
        : null;
    case "connecting":
      return typeof json.attempt === "number" &&
        (json.last_error === null || isStr(json.last_error))
        ? ok({
            state: "connecting",
            attempt: json.attempt,
            last_error: json.last_error,
          })
        : null;
    case "pending":
      return typeof json.empty_account === "boolean" &&
        typeof json.rejected === "boolean" &&
        (json.sas === null || isSix(json.sas))
        ? ok({
            state: "pending",
            empty_account: json.empty_account,
            sas: json.sas,
            rejected: json.rejected,
          })
        : null;
    case "active":
      return Array.isArray(json.devices) &&
        json.devices.every(isDevice) &&
        Array.isArray(json.requests) &&
        json.requests.every(isRequest)
        ? ok({ state: "active", devices: json.devices, requests: json.requests })
        : null;
    case "trust":
      return isStr(json.trust_error) && TRUST_ERRORS.includes(json.trust_error)
        ? ok({ state: "trust", trust_error: json.trust_error as "fork" })
        : null;
    default:
      return null;
  }
}
