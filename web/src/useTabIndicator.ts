// The tab is the working indicator: while the agent runs, the sage favicon
// turns terracotta and the title gains a "⟳ " badge, so a backgrounded tab
// shows progress without an animation loop. Both are restored exactly when
// the run stops (or the component unmounts).

import { useEffect } from "react";

const IDLE = "/logo_shifu.png";
const RUNNING = "/logo_shifu_think.png";
const BADGE = "⟳ ";

// `rel~='icon'` matches a whole token, so it can never select apple-touch-icon.
function iconLink(): HTMLLinkElement {
  const existing = document.querySelector<HTMLLinkElement>("link[rel~='icon']");
  if (existing) return existing;
  const link = document.createElement("link");
  link.rel = "icon";
  document.head.appendChild(link);
  return link;
}

export function useTabIndicator(isRunning: boolean) {
  // Warm the cache once, or the first swap blanks the tab while the PNG loads.
  useEffect(() => {
    new Image().src = RUNNING;
  }, []);

  useEffect(() => {
    // Idle needs no work: index.html ships the idle icon and plain title, and
    // the cleanup below restores both.
    if (!isRunning) return;
    const link = iconLink();
    const title = document.title;
    link.href = RUNNING;
    document.title = BADGE + title;
    return () => {
      link.href = IDLE;
      document.title = title;
    };
  }, [isRunning]);
}
