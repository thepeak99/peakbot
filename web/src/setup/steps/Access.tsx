// Step 7 — Access. Local or LAN; bind + token are not written to
// config.yaml (plan §A-Q5) — they live on the launch command line and in
// $PEAKBOT_WEB_TOKEN. The token rides in the draft so the Start-on-boot
// step can pass it to POST /api/setup/service.

import type { AccessDraft } from "../draft";
import { validateAccess } from "../draft";
import type { StepProps } from "../steps";
import { Check, Field, RadioCards, ghostButtonClass, inputClass } from "../ui";

function generateToken(): string {
  return crypto.randomUUID().replaceAll("-", "");
}

export function AccessStep({ draft, patch, info }: StepProps) {
  const set = (partial: Partial<AccessDraft>) => patch({ access: { ...draft.access, ...partial } });
  const { mode, bindAddress, token, tls } = draft.access;
  const tokenMissing = validateAccess(draft).length > 0;
  const lanHint = info?.lan_bind_hint ?? "0.0.0.0:7823";
  const launch = mode === "lan" ? `peakbot --bind ${bindAddress ?? lanHint}` : "peakbot";

  return (
    <div className="space-y-4">
      <RadioCards<"local" | "lan">
        name="access-mode"
        value={mode}
        onChange={(next) => set(next === "lan" ? { mode: next, bindAddress: bindAddress ?? lanHint } : { mode: next })}
        options={[
          { value: "local", label: "Local only", hint: "Binds 127.0.0.1:7823. No token needed." },
          { value: "lan", label: "Reachable from other devices", hint: "Phones and laptops on your network. Token required." },
        ]}
      />

      {mode === "lan" && (
        <div className="space-y-4 rounded-lg border border-zinc-800 p-3">
          <Field label="Bind address" hint="host:port the server listens on.">
            <input value={bindAddress ?? ""} onChange={(e) => set({ bindAddress: e.target.value })} placeholder={lanHint} spellCheck={false} className={inputClass} />
          </Field>
          <Field label="Token" hint="Kept in $PEAKBOT_WEB_TOKEN or a 0600 file next to config.yaml." error={tokenMissing ? "Required for a non-loopback bind." : null}>
            <div className="flex gap-2">
              <input value={token ?? ""} onChange={(e) => set({ token: e.target.value })} autoComplete="off" spellCheck={false} placeholder="paste or generate" className={`${inputClass} font-mono text-xs`} />
              <button type="button" onClick={() => set({ token: generateToken() })} className={`${ghostButtonClass} shrink-0`}>Generate</button>
            </div>
          </Field>
          <div className="space-y-2">
            <Check label="Serve over HTTPS (built-in CA)" hint="Self-signed CA, fresh leaf each boot. TLS complements the token, never replaces it." checked={tls ?? false} onChange={(next) => set({ tls: next })} />
            {tls && (
              <p className="text-[11px] text-zinc-500">
                Install the CA on phones from <code className="text-zinc-400">/peakbot-ca.crt</code> — on iOS also trust it under Settings → General → About → Certificate Trust Settings.
              </p>
            )}
          </div>
        </div>
      )}

      <div className="space-y-1 rounded-lg border border-zinc-800 p-3 text-xs text-zinc-400">
        <p className="font-medium text-zinc-300">Launch command</p>
        <pre className="overflow-x-auto rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-1.5 text-xs text-zinc-200">{launch}</pre>
        {mode === "lan" && token && (
          <p className="text-[11px] text-zinc-500">Token is read from <code className="text-zinc-400">$PEAKBOT_WEB_TOKEN</code> or a 0600 file next to <code className="text-zinc-400">config.yaml</code>.</p>
        )}
      </div>
    </div>
  );
}
