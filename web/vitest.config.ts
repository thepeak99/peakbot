// Vitest configuration for the web package. Sits beside `vite.config.ts` so the
// `vite build` target keeps its dev-server / Tailwind setup, while vitest gets
// its own resolver. We do NOT enable the jsdom environment globally — most of
// the suite is pure logic (adapt, transcriptRows, transcriptEpoch,
// useTranscriptScroll, ConversationsPicker) and runs ~3× faster under
// `node`. Only the component-level DOM tests opt in via the per-file glob
// below.
//
// `environmentMatchGlobs` is the matching knob for that opt-in: any test file
// under `src/components/` runs under jsdom so future component tests inherit
// the environment automatically. Pure-logic files keep `node`.

import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environmentMatchGlobs: [["src/components/**", "jsdom"]],
  },
});
