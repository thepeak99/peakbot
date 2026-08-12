// The transcript pane with nothing in it. Split out from `Transcript` so the
// virtualizer never has to reason about zero rows: App branches on the
// message count and only one of the two can be mounted.

import { WelcomeBanner } from "./WelcomeBanner";
import type { Welcome } from "../types";

interface Props {
  /** The startup banner, or null once the conversation has any message. */
  welcome: Welcome | null;
  /** The watched sub-agent lane, or null in the global view. */
  scopeLabel: string | null;
}

export function EmptyTranscript({ welcome, scopeLabel }: Props) {
  return (
    <section className="min-h-0 flex-1 overflow-y-auto mr-12 px-4 py-4 sm:px-6 md:px-8">
      <div className="mx-auto max-w-5xl space-y-3">
        {welcome && <WelcomeBanner welcome={welcome} />}
        {scopeLabel && (
          <p className="rounded-md border border-dashed border-zinc-800 px-3 py-6 text-center text-xs text-zinc-600">
            No messages from {scopeLabel} yet.
          </p>
        )}
      </div>
    </section>
  );
}
