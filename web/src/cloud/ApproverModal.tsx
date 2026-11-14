// P4 — the global approver modal (design §A.2).
//
// Derives the "current request" = the earliest-expiring of `active.requests`
// that is not dismissed this session and not expired; renders nothing
// otherwise. All the click-only safety lives in `SafeDialog`.

import { useEffect, useState } from "react";
import { COPY } from "./copy";
import { store } from "./store";
import { SafeDialog } from "./SafeDialog";
import { SasWords } from "./SasWords";
import { useCloud } from "./useCloud";
import type { EnrollRequest } from "./types";

export function ApproverModal() {
  const { snapshot, dismissed } = useCloud();
  // Local tick for the "expires in m:ss" sub-line.
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const t = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    return () => window.clearInterval(t);
  }, []);

  const active = snapshot?.state === "active" ? snapshot : null;
  const req: EnrollRequest | null =
    (active &&
      [...active.requests]
        .filter((r) => !dismissed.has(r.letter_id) && r.expires_at > now)
        .sort((a, b) => a.expires_at - b.expires_at)[0]) ??
    null;
  if (!req) return null;

  const left = Math.max(0, req.expires_at - now);
  const meta = COPY.approve.meta(req.platform, Math.floor(left / 60), left % 60);

  return (
    <SafeDialog
      title={COPY.approve.title(req.name)}
      onLater={() => store.dismiss(req.letter_id)}
      actions={[
        {
          label: COPY.btn.later,
          kind: "neutral",
          onClick: async () => {
            store.dismiss(req.letter_id);
          },
        },
        {
          label: COPY.btn.reject,
          kind: "danger",
          onClick: async () => {
            await store.act("/api/cloud/enroll/reject", { letter_id: req.letter_id });
          },
        },
        {
          label: COPY.btn.approve,
          kind: "primary",
          onClick: async () => {
            await store.act("/api/cloud/enroll/approve", { letter_id: req.letter_id });
          },
        },
      ]}
    >
      <p>{COPY.approve.body}</p>
      <SasWords words={req.sas} />
      <p aria-hidden className="text-xs text-zinc-400">
        {meta}
      </p>
    </SafeDialog>
  );
}
