// The tab icon is the working indicator: the sage turns terracotta while the
// agent runs, so a backgrounded tab shows progress without an animation loop.

import { useEffect } from "react";

const IDLE = "/logo_shifu.png";
const RUNNING = "/logo_shifu_think.png";

// `rel~='icon'` matches a whole token, so it can never select apple-touch-icon.
function iconLink(): HTMLLinkElement {
  const existing = document.querySelector<HTMLLinkElement>("link[rel~='icon']");
  if (existing) return existing;
  const link = document.createElement("link");
  link.rel = "icon";
  document.head.appendChild(link);
  return link;
}

export function useFavicon(isRunning: boolean) {
  // Warm the cache once, or the first swap blanks the tab while the PNG loads.
  useEffect(() => {
    new Image().src = RUNNING;
  }, []);

  useEffect(() => {
    const link = iconLink();
    link.href = isRunning ? RUNNING : IDLE;
    // Reusing the one link element keeps the icon count flat across toggles, and
    // the cleanup stops an unmount from stranding the running icon on the tab.
    return () => {
      link.href = IDLE;
    };
  }, [isRunning]);
}
