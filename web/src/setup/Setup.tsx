/**
 * Setup — the /setup wizard frame.
 *
 * Owns the one and only piece of state: a single `SetupDraft` in `useState`.
 * Nothing persists — a refresh starts over, which is why every fake action
 * carries a `preview` chip (plan §8.1).
 *
 * It deliberately does NOT touch `useAgent`: the wizard has to render before
 * any WebSocket exists, and keeping that boundary is what makes a first-run
 * redirect trivial later.
 */

import { useEffect, useState } from "react";
import { ThemeToggle } from "../components/ThemeToggle";
import { defaultSetupDraft, configJsonToDraft, type SetupDraft } from "./draft";
import { REVIEW_INDEX, STEPS } from "./steps";
import { Errors, buttonClass, ghostButtonClass } from "./ui";
import { apiErrorMessage, getSetupInfo, type SetupInfo } from "./api";

/** Rail dot: red on a hard error, filled once the step has real input. */
function dotClass(state: "error" | "complete" | "empty"): string {
  if (state === "error") return "bg-red-500";
  if (state === "complete") return "bg-emerald-500";
  return "border border-zinc-700";
}

export function Setup() {
  const [draft, setDraft] = useState<SetupDraft>(defaultSetupDraft);
  const [index, setIndex] = useState(0);
  const [visited, setVisited] = useState<number[]>([0]);
  const [info, setInfo] = useState<SetupInfo | null>(null);
  const [infoError, setInfoError] = useState<string[]>([]);

  useEffect(() => {
    let cancelled = false;
    getSetupInfo()
      .then((next) => {
        if (cancelled) return;
        setInfo(next);
        const existing = next.existing;
        if (existing.status === "ok") {
          setDraft(configJsonToDraft(existing.config));
          setDraft((d) => ({ ...d, welcome: { ...d.welcome, startMode: "import", importedSummary: "Imported from existing config.yaml" } }));
        } else if (existing.status === "error") {
          setInfoError((prev) => [...prev, `Existing config could not be parsed: ${existing.message}. Starting with a blank draft.`]);
        }
      })
      .catch((err) => { if (!cancelled) setInfoError(apiErrorMessage(err)); });
    return () => { cancelled = true; };
  }, []);

  const step = STEPS[index];
  const errors = step.errors(draft);

  const patch = (partial: Partial<SetupDraft>) =>
    setDraft((d) => ({ ...d, ...partial }));

  const goTo = (next: number) => {
    setIndex(next);
    setVisited((v) => (v.includes(next) ? v : [...v, next]));
  };

  // Back returns to the last step you actually visited, so jumping straight to
  // Review doesn't strand you on the optional steps you skipped.
  const previous = visited.filter((i) => i < index).sort((a, b) => a - b).pop();

  return (
    <div className="flex h-dvh w-full flex-col overflow-hidden bg-zinc-950 text-zinc-100">
      <header className="flex items-center gap-3 border-b border-zinc-900 px-4 py-2.5">
        <img src="/favicon.svg" alt="" className="h-6 w-6" />
        <h1 className="text-sm font-medium">Setup</h1>
        {info && (
          <span className="text-[11px] text-zinc-500">
            {info.os} · {info.arch}
            {info.needs_setup ? " · first run" : null}
          </span>
        )}
        <div className="ml-auto flex items-center gap-3">
          <a
            href="/"
            className="text-xs text-zinc-500 transition-colors hover:text-zinc-300"
          >
            Back to chat
          </a>
          <ThemeToggle />
        </div>
      </header>

      <div className="flex min-h-0 flex-1">
        {/* Desktop rail. Below sm it collapses to the progress header below. */}
        <nav className="hidden w-56 shrink-0 overflow-y-auto border-r border-zinc-900 p-3 sm:block">
          <ol className="space-y-0.5">
            {STEPS.map((s, i) => {
              const hasErrors = s.errors(draft).length > 0;
              const state = hasErrors
                ? "error"
                : s.isComplete(draft)
                  ? "complete"
                  : "empty";
              const reachable = visited.includes(i);
              return (
                <li key={s.id}>
                  <button
                    type="button"
                    disabled={!reachable}
                    onClick={() => goTo(i)}
                    className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors ${
                      i === index
                        ? "bg-zinc-900 text-zinc-100"
                        : reachable
                          ? "cursor-pointer text-zinc-400 hover:bg-zinc-900/60 hover:text-zinc-200"
                          : "cursor-not-allowed text-zinc-600"
                    }`}
                  >
                    <span
                      className={`h-1.5 w-1.5 shrink-0 rounded-full ${dotClass(state)}`}
                    />
                    <span className="truncate">
                      {i + 1}. {s.title}
                    </span>
                    {s.optional && (
                      <span className="ml-auto text-[10px] text-zinc-600">
                        optional
                      </span>
                    )}
                  </button>
                </li>
              );
            })}
          </ol>
        </nav>

        <main className="flex min-w-0 flex-1 flex-col">
          <div className="border-b border-zinc-900 px-4 py-2 sm:hidden">
            <p className="text-xs text-zinc-400">
              Step {index + 1} of {STEPS.length} · {step.title}
            </p>
            <div className="mt-1.5 h-1 w-full overflow-hidden rounded-full bg-zinc-900">
              <div
                className="h-full bg-zinc-600 transition-all"
                style={{ width: `${((index + 1) / STEPS.length) * 100}%` }}
              />
            </div>
          </div>

          <section className="min-h-0 flex-1 overflow-y-auto px-4 py-4 sm:px-6">
            <div className="mx-auto max-w-3xl space-y-4">
              <div className="hidden items-baseline gap-2 sm:flex">
                <h2 className="text-base font-medium text-zinc-100">
                  {step.title}
                </h2>
                {step.optional && (
                  <span className="text-[11px] text-zinc-500">
                    optional — skip it if you like
                  </span>
                )}
              </div>
              <Errors errors={errors} />
              <Errors errors={infoError} />
              <step.Component
                draft={draft}
                patch={patch}
                next={() => goTo(Math.min(index + 1, REVIEW_INDEX))}
                info={info}
              />
            </div>
          </section>

          <footer className="flex items-center gap-2 border-t border-zinc-900 px-4 py-2.5 sm:px-6">
            <button
              type="button"
              disabled={previous === undefined}
              onClick={() => previous !== undefined && goTo(previous)}
              className={ghostButtonClass}
            >
              Back
            </button>
            <div className="ml-auto flex items-center gap-2">
              {index < REVIEW_INDEX && (
                <button
                  type="button"
                  disabled={errors.length > 0}
                  onClick={() => goTo(REVIEW_INDEX)}
                  className={ghostButtonClass}
                >
                  Skip to review
                </button>
              )}
              {index < REVIEW_INDEX && (
                <button
                  type="button"
                  disabled={errors.length > 0}
                  onClick={() => goTo(index + 1)}
                  className={buttonClass}
                >
                  Next
                </button>
              )}
            </div>
          </footer>
        </main>
      </div>
    </div>
  );
}
