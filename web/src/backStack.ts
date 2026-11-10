// The one arbiter of "Back closes the top layer" (design §3.4). Every
// history operation in the app goes through here, so nested overlays never
// both react to one popstate, and a layer's own history.back() never fires
// another layer's onBack.

interface Layer {
  onBack: () => void;
  /** Open, but its marker entry is not pushed yet (see pushBackEntry). */
  owed: boolean;
}

const stack: Layer[] = [];
/** Popstates still to come from our own history.back() calls. */
let swallow = 0;
let installed = false;

function pushMarker(): void {
  window.history.pushState(window.history.state, "");
}

function ensureListener(): void {
  if (installed) return;
  installed = true;
  window.addEventListener("popstate", () => {
    if (swallow === 0) {
      stack.pop()?.onBack();
      return;
    }
    swallow--;
    if (swallow > 0) return;
    for (const layer of stack) {
      if (!layer.owed) continue;
      layer.owed = false;
      pushMarker();
    }
  });
}

/** Push one marker entry (URL unchanged). `onBack` runs if the user's Back
 *  consumes it. The returned `release` consumes the marker when the layer
 *  closes any other way; it is idempotent and a no-op after a Back. */
export function pushBackEntry(onBack: () => void): () => void {
  ensureListener();
  // Invariant: no pushState while a release's back() is in flight. The
  // traversal would strand the new marker as forward history (jsdom cancels
  // the traversal instead), so the marker is owed until that popstate lands.
  const layer: Layer = { onBack, owed: swallow > 0 };
  if (!layer.owed) pushMarker();
  stack.push(layer);
  return () => {
    const idx = stack.indexOf(layer);
    if (idx === -1) return; // consumed by a Back, or already released
    stack.splice(idx, 1);
    if (layer.owed) return; // no marker was pushed: nothing to consume
    swallow++;
    window.history.back();
  };
}
