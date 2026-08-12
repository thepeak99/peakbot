/** Derives the remount token for the transcript. When the epoch changes, the
 *  transcript component remounts (fresh virtualizer, fresh scroll element) —
 *  stronger than clearing caches, with no ordering hazards. */

export interface EpochState { id: string; count: number; epoch: number }

export function nextEpoch(prev: EpochState, id: string, count: number): EpochState {
  if (id !== prev.id) return { id, count, epoch: prev.epoch + 1 };
  if (count < prev.count) return { id, count, epoch: prev.epoch + 1 }; // truncation: indices now point elsewhere
  return count === prev.count ? prev : { ...prev, count };
}

export const epochKey = (s: EpochState): string => `${s.id}#${s.epoch}`;
