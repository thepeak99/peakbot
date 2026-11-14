// P4 — the ONE click-only dialog primitive (design §E).
//
// `ApproverModal` and the three confirms both render through it, so the safety
// rules live in one place and are tested once:
//  • initial focus → the first `neutral` action (never a danger/primary);
//  • Enter is swallowed (capture phase) — it never "clicks" a focused button;
//  • Escape → `onLater()` (never a reject);
//  • Tab/Shift+Tab wrap within the dialog's enabled buttons;
//  • `role="dialog"`/`alertdialog`, `aria-modal`, labelled/described;
//  • all action buttons `disabled` while the shared store is `busy`.
//
// SKELETON: renders nothing; `SafeDialog`/`ApproverModal` safety tests are RED.

import type { ReactNode } from "react";

export type SafeAction = {
  label: string;
  kind: "neutral" | "danger" | "primary";
  onClick: () => Promise<void>;
};

export type SafeDialogProps = {
  title: string;
  children: ReactNode;
  actions: SafeAction[];
  onLater: () => void;
};

export function SafeDialog(_props: SafeDialogProps): null {
  void _props;
  return null;
}
