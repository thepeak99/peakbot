// P4 — the red, non-dismissible trust banner (design §A.4).
//
// Full-width bar at the very top of the shell when `state === "trust"`:
// `role="alert"`, a per-`trust_error` string, and an "Open Cloud settings"
// button (a real `<button>`, not an anchor). Renders `null` in every other
// state.

import { COPY } from "./copy";
import { openCloudSettings } from "./openCloud";
import { useCloud } from "./useCloud";

export function TrustBanner() {
  const { snapshot } = useCloud();
  if (!snapshot || snapshot.state !== "trust") return null;
  return (
    <div role="alert" className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 bg-red-700 px-4 py-2 text-sm text-white">
      <span>{COPY.banner[snapshot.trust_error]}</span>
      <button
        type="button"
        onClick={() => openCloudSettings()}
        className="shrink-0 rounded border border-white/40 px-2 py-0.5 text-xs font-medium hover:bg-red-600"
      >
        {COPY.btn.openCloudSettings}
      </button>
    </div>
  );
}
