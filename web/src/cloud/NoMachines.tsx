// P5 — the zero-local zero-cloud empty state (design §4).
// Shown when noLocal is true and there are no cloud targets.

import type { JSX } from "react";
import { COPY } from "./copy";
import { openCloudSettings } from "./openCloud";

export function NoMachines(): JSX.Element {
  return (
    <div className="no-machines">
      <p>{COPY.empty.noMachines}</p>
      <button onClick={openCloudSettings}>{COPY.btn.openCloudSettings}</button>
    </div>
  );
}