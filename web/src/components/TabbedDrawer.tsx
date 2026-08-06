// Off-canvas drawer with vertical tab handles — one drawer, many contents.
// Adapted from AnimAI's TabbedDrawer. The whole rail (handles + body) is
// anchored to the right edge; closed, it slides right by the body width so
// only the handle column stays flush to the viewport. Handle semantics:
//   closed          → clicking any handle opens that tab
//   open, other tab → switches to it (stays open)
//   open, same tab  → closes the drawer
//
// Purely presentational: each tab supplies its own content. Replaces the old
// static `<aside>` + separate mobile drawer — one responsive mechanism now.
// `--drawer-w` (body width) = min(width, 94vw), so the transform that hides
// the body and the body's own width always match on any viewport.

import { useEffect, type ReactNode } from "react";
import { useMediaQuery } from "../useMediaQuery";

export interface DrawerTab {
  id: string;
  /** Optional emoji handle glyph. Omit for a text-only handle. */
  icon?: string;
  /** Short vertical label ("SESSION", "TODO"…). */
  label: string;
  content: ReactNode;
  /** Optional badge (e.g. a count) rendered on the handle. */
  badge?: number;
}

export function TabbedDrawer({
  tabs,
  active,
  onActiveChange,
  width = 288,
  defaultTab = null,
}: {
  tabs: DrawerTab[];
  active: string | null;
  onActiveChange: (active: string | null) => void;
  /** Drawer body width in px; capped at 94vw so it fits small phones. */
  width?: number;
  defaultTab?: string | null;
}) {
  void defaultTab;
  // Tap-outside-to-close is a touch affordance. On desktop the drawer is a
  // slim side rail you keep open while working — closing it on any stray click
  // in the transcript would be surprising, so it's mobile/tablet only.
  const isDesktop = useMediaQuery("(min-width: 1024px)");

  // Escape closes the drawer.
  useEffect(() => {
    if (active === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onActiveChange(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [active, onActiveChange]);

  if (tabs.length === 0) return null;

  const open = active !== null;
  const activeTab = tabs.find((t) => t.id === active);
  const toggle = (id: string) => onActiveChange(active === id ? null : id);

  return (
    <>
      {/* Tap/click outside the drawer closes it — mobile/tablet only. On
          desktop the drawer stays a persistent side rail. Transparent overlay
          sits below the drawer (z-30 < z-40) so the handle rail and body stay
          clickable; everything else routes its click here. */}
      {open && !isDesktop && (
        <div
          className="fixed inset-0 z-30"
          onClick={() => onActiveChange(null)}
          aria-hidden
        />
      )}
      <aside
        className="fixed right-0 top-14 bottom-14 z-40 flex items-start transition-transform duration-300 ease-out lg:bottom-0"
        style={{
          ["--drawer-w" as string]: `min(${width}px, 94vw)`,
          transform: open ? "translateX(0)" : "translateX(var(--drawer-w))",
        }}
        aria-label="Side panels"
      >
      {/* Handle rail — first flex child, so it sits left of the body and stays
          flush to the viewport edge when the body is pushed off-screen. */}
      <div role="tablist" className="flex flex-col gap-1.5 pt-1">
        {tabs.map((t) => {
          const isActive = active === t.id;
          return (
            <button
              key={t.id}
              role="tab"
              aria-selected={isActive}
              onClick={() => toggle(t.id)}
              title={isActive ? `Close ${t.label}` : `Open ${t.label}`}
              className={`flex w-10 cursor-pointer flex-col items-center gap-1.5 rounded-l-lg border py-3 shadow-md transition-colors ${
                isActive
                  ? "border-zinc-700 bg-zinc-800 text-zinc-100"
                  : "border-zinc-800 bg-zinc-900/90 text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200"
              }`}
            >
              {t.icon && (
                <span className="text-base leading-none">{t.icon}</span>
              )}
              <span className="[writing-mode:vertical-rl] text-[10px] font-semibold uppercase tracking-widest">
                {t.label}
              </span>
              {t.badge != null && t.badge > 0 && (
                <span className="rounded-full bg-sky-600 px-1.5 font-mono text-[10px] leading-4 text-white">
                  {t.badge}
                </span>
              )}
            </button>
          );
        })}
      </div>

      {/* Drawer body — only the active tab's content. Hidden from a11y/scroll
          when closed. */}
      <div
        role="tabpanel"
        aria-hidden={!open}
        className={`flex h-full flex-col gap-5 overflow-y-auto border-l border-zinc-800 bg-zinc-950/95 p-4 ${
          open ? "" : "invisible"
        }`}
        style={{ width: "var(--drawer-w)" }}
      >
        {activeTab?.content}
      </div>
      </aside>
    </>
  );
}
