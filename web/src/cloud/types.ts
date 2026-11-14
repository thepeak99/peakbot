// P4 — cloud wire types (design §1) + the ONE boundary parser.
//
// RED before implementation: the types are the contract, but
// `parseStateBody` is a skeleton that throws, so `types.test.ts` fails for
// the right reason (feature absent).

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
      trust_error: "genesis_mismatch" | "fork" | "revoked" | "chain_invalid";
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

/** The ONE boundary parser: the only place a raw SSE/JSON payload is checked.
 *
 * Contract (design §B): checks the `state` tag, `sas.length === 6`, and that
 * a `pending` body carries a `rejected` boolean (strict — the server always
 * sends it). Unknown tag / malformed → `null` (treated as "no snapshot").
 *
 * SKELETON: not implemented yet.
 */
export function parseStateBody(_json: unknown): StateBody | null {
  void _json;
  throw new Error("not implemented");
}
