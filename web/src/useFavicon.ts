// Dynamically swap the favicon to a spinning yellow arc while the agent is
// working, and restore the original icon when idle.

import { useEffect, useRef } from "react";

const TICK_MS = 66;

export function useFavicon(isRunning: boolean) {
  const cancelRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    // Always tear down the previous run before starting a new one.
    cancelRef.current?.();
    cancelRef.current = null;

    const setHref = (href: string, type?: string) => {
      const link = document.createElement("link");
      link.rel = "icon";
      if (type) link.type = type;
      link.href = href;
      document.head.appendChild(link);
      return link;
    };

    if (!isRunning) {
      // Adopt the existing favicon's href if one is already on the page so the
      // HTML <link rel="icon"> stays the single source of truth.
      const existing = document.querySelector<HTMLLinkElement>("link[rel~='icon']");
      setHref(existing?.href ?? "/favicon.svg");
      return;
    }

    const canvas = document.createElement("canvas");
    canvas.width = 16;
    canvas.height = 16;
    const ctx = canvas.getContext("2d")!;
    const link = setHref("", "image/png");

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

    const cancel = () => {
      clearInterval(id);
      link.remove();
    };
    cancelRef.current = cancel;
    return cancel;
  }, [isRunning]);
}