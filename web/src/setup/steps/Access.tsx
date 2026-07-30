// Step 7 — Access. Local only, or reachable from other devices.
//
// Choosing LAN reveals the bind address and *forces* a token: a non-loopback
// bind without one is refused by the binary, so the wizard refuses it too
// (validateAccess → Next is disabled). Generate produces a real random string
// client-side; it just isn't saved anywhere in this preview.

import type { AccessDraft } from "../draft";
import { validateAccess } from "../draft";
import type { StepProps } from "../steps";
import { PLATFORM } from "../fixtures";
import {
  Check,
  Field,
  RadioCards,
  ghostButtonClass,
  inputClass,
} from "../ui";

/** A token you can paste into a URL: UUID entropy, no dashes. */
function generateToken(): string {
  return crypto.randomUUID().replaceAll("-", "");
}

export function AccessStep({ draft, patch }: StepProps) {
  const set = (partial: Partial<AccessDraft>) =>
    patch({ access: { ...draft.access, ...partial } });
  const { mode, bindAddress, token, tls } = draft.access;
  const tokenMissing = validateAccess(draft).length > 0;

  return (
    <div className="space-y-4">
      <RadioCards<"local" | "lan">
        name="access-mode"
        value={mode}
        onChange={(next) =>
          set(
            next === "lan"
              ? { mode: next, bindAddress: bindAddress ?? PLATFORM.lanBind }
              : { mode: next },
          )
        }
        options={[
          {
            value: "local",
            label: "Local only",
            hint: "Binds 127.0.0.1:7823. No token needed.",
          },
          {
            value: "lan",
            label: "Reachable from other devices",
            hint: "Phones and laptops on your network. Token required.",
          },
        ]}
      />

      {mode === "lan" && (
        <div className="space-y-4 rounded-lg border border-zinc-800 p-3">
          <Field label="Bind address" hint="host:port the server listens on.">
            <input
              value={bindAddress ?? ""}
              onChange={(e) => set({ bindAddress: e.target.value })}
              placeholder={PLATFORM.lanBind}
              spellCheck={false}
              className={inputClass}
            />
          </Field>

          <Field
            label="Token"
            hint="Guards every route. Presented once as ?token=… , then kept in a cookie."
            error={tokenMissing ? "Required for a non-loopback bind." : null}
          >
            <div className="flex gap-2">
              <input
                value={token ?? ""}
                onChange={(e) => set({ token: e.target.value })}
                autoComplete="off"
                spellCheck={false}
                placeholder="paste or generate"
                className={`${inputClass} font-mono text-xs`}
              />
              <button
                type="button"
                onClick={() => set({ token: generateToken() })}
                className={`${ghostButtonClass} shrink-0`}
              >
                Generate
              </button>
            </div>
          </Field>

          <div className="space-y-2">
            <Check
              label="Serve over HTTPS (built-in CA)"
              hint="Self-signed CA, fresh leaf each boot. TLS complements the token, never replaces it."
              checked={tls ?? false}
              onChange={(next) => set({ tls: next })}
            />
            {tls && (
              <p className="text-[11px] text-zinc-500">
                Install the CA on phones from{" "}
                <code className="text-zinc-400">/peakbot-ca.crt</code> — on iOS
                also trust it under Settings → General → About → Certificate
                Trust Settings.
              </p>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
