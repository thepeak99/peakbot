// Dynamically swap the favicon to a spinning yellow arc while the agent is
// working, and restore the original icon when idle.

import { useEffect } from "react";

const TICK_MS = 66;

export function useFavicon(isRunning: boolean) {
  useEffect(() => {
    if (!isRunning) {
      // Replace the single existing <link rel="icon"> in place so the browser
      // re-reads it and the count never grows across toggles. The HTML
      // <link rel="icon"> stays the source of truth for the idle href.
      const existing = document.querySelector<HTMLLinkElement>("link[rel~='icon']");
      const href = existing?.href ?? "/favicon.svg";
      existing?.remove();
      const link = document.createElement("link");
      link.rel = "icon";
      link.href = href;
      document.head.appendChild(link);
      return;
    }

    const canvas = document.createElement("canvas");
    canvas.width = 16;
    canvas.height = 16;
    const ctx = canvas.getContext("2d")!;
    const link = document.createElement("link");
    link.rel = "icon";
    link.type = "image/png";
    document.head.appendChild(link);

    let angle = 0;
    const id = window.setInterval(() => {
      ctx.clearRect(0, 0, 16, 16);
      ctx.beginPath();
      ctx.arc(8, 8, 7, angle, angle + Math.PI * 1.5);
      ctx.strokeStyle = "#facc15";
      ctx.lineWidth = 2;
      ctx.lineCap = "round";
      ctx.stroke();
      link.href = canvas.toDataURL("image/png");
      angle = (angle + 0.15) % (Math.PI * 2);
    }, TICK_MS);

    // React runs this cleanup before the next effect and on unmount, so the
    // animated link and its interval are always torn down — no ref needed.
    return () => {
      clearInterval(id);
      link.remove();
    };
  }, [isRunning]);
}
