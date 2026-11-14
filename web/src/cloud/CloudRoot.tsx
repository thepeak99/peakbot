// P4 — the one thing `App.tsx` mounts (design §B): `<TrustBanner/>` +
// `<ApproverModal/>`. Always mounted, so the SSE subscription lives for the
// app's lifetime; when `available === false` it renders nothing.
// SKELETON: renders nothing.

export function CloudRoot(): null {
  return null;
}
