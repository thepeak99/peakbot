// One keep-alive composer per desktop tab. Inactive tabs render a parked,
// hidden <Composer> instead of unmounting, so each tab keeps its own text,
// slash-palette state and attachments (in-flight uploads included) in plain
// React state. The slot owns the tab's `useAttachments`; its `convo` is the
// tab's own, so uploads stay under the right conversation while parked.

import { memo, useImperativeHandle, type Ref } from "react";
import type { UploadLimits } from "../state";
import { useAttachments, DEFAULT_UPLOAD_LIMITS } from "../useAttachments";
import { Composer, type ComposerProps } from "./Composer";

export type ComposerLive = Omit<ComposerProps, "attachments" | "hidden"> & {
  uploadLimits: UploadLimits;
};

export interface ComposerSlotHandle {
  add(files: File[]): void;
}

const noop = () => {};

// What an inactive tab's composer renders: nothing live, nothing clickable.
const PARKED: ComposerLive = {
  isRunning: false,
  connected: false,
  commands: [],
  onSend: noop,
  onStop: noop,
  onPause: noop,
  onResume: noop,
  pendingInput: 0,
  uploadLimits: DEFAULT_UPLOAD_LIMITS,
};

export const ComposerSlot = memo(function ComposerSlot({
  convo,
  live,
  ref,
}: {
  convo: string | null;
  /** `null` = inactive tab: parked and hidden. */
  live: ComposerLive | null;
  ref?: Ref<ComposerSlotHandle>;
}) {
  const { uploadLimits, ...props } = live ?? PARKED;
  const attachments = useAttachments(convo, uploadLimits);
  useImperativeHandle(ref, () => ({ add: attachments.add }), [attachments.add]);
  return <Composer {...props} hidden={live === null} attachments={attachments} />;
});
