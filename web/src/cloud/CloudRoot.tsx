// P4 — the one thing `App.tsx` mounts (design §B): `<TrustBanner/>` +
// `<ApproverModal/>`. Always mounted, so the SSE subscription lives for the
// app's lifetime; when `available === false` it renders nothing.

import { ApproverModal } from "./ApproverModal";
import { TrustBanner } from "./TrustBanner";
import { useCloud } from "./useCloud";

export function CloudRoot() {
  const { available } = useCloud();
  if (!available) return null;
  return (
    <>
      <TrustBanner />
      <ApproverModal />
    </>
  );
}
