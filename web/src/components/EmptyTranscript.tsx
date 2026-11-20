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
    <section className="flex min-h-0 flex-1 flex-col overflow-y-auto desktop:mr-12 px-4 py-4 sm:px-6 md:px-8">
      <div className="mx-auto w-full max-w-5xl space-y-3">
        {welcome && <WelcomeBanner welcome={welcome} />}
        {scopeLabel && (
          <p className="rounded-md border border-dashed border-zinc-800 px-3 py-6 text-center text-xs text-zinc-600 phone:border-0 phone:text-[14.5px] phone:text-zinc-500">
            No messages from {scopeLabel} yet.
          </p>
        )}
      </div>
      {/* The mock's centred `.empty` hint; phone-only so desktop is unchanged. */}
      {!scopeLabel && (
        <p className="my-auto p-6 text-center text-[14.5px] leading-[1.6] text-zinc-500 desktop:hidden">
          Nothing here yet —
          <br />
          ask for something.
        </p>
      )}
    </section>
  );
}
