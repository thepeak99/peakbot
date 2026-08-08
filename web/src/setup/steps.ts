/**
 * steps.ts — the STEPS array, and the only contract a step has to satisfy.
 *
 * Adding a step is one entry plus one component. That is the whole extension
 * story: no schema-driven form generator, no plugin registry (plan §8.6).
 *
 * `errors` returns *hard* errors — the ones that disable Next. An unfilled
 * optional field is not an error; a duplicate alias is.
 */

import type { SetupInfo } from "./api";
import type { ReactElement } from "react";
import type { SetupDraft } from "./draft";
import {
  validateAccess,
  validateDraft,
  validateModels,
  validateMultiAgent,
  validateServices,
} from "./draft";
import { WelcomeStep } from "./steps/Welcome";
import { LocationsStep } from "./steps/Locations";
import { ProviderStep } from "./steps/Provider";
import { ModelsStep } from "./steps/Models";
import { PersonaStep } from "./steps/Persona";
import { ServicesStep } from "./steps/Services";
import { AccessStep } from "./steps/Access";
import { StartOnBootStep } from "./steps/StartOnBoot";
import { MultiAgentStep } from "./steps/MultiAgent";
import { ReviewStep } from "./steps/Review";

export type StepProps = {
  draft: SetupDraft;
  /** Merge a slice into the draft. Steps spread their own slice. */
  patch: (partial: Partial<SetupDraft>) => void;
  /** Advance one step — for the in-body shortcuts (Welcome's "Start fresh"). */
  next: () => void;
  /** Live machine facts from GET /api/setup (null until the fetch resolves). */
  info: SetupInfo | null;
};

export type Step = {
  id: string;
  title: string;
  /** Skippable steps (plan §8.4: 6–9), flagged in the rail. */
  optional?: boolean;
  /** Has the user given this step meaningful input? Drives the rail dot. */
  isComplete: (draft: SetupDraft) => boolean;
  errors: (draft: SetupDraft) => string[];
  Component: (props: StepProps) => ReactElement;
};

const none = () => [];

export const STEPS: Step[] = [
  {
    id: "welcome",
    title: "Welcome",
    isComplete: (d) => !!d.welcome.startMode,
    errors: none,
    Component: WelcomeStep,
  },
  {
    id: "locations",
    title: "Locations",
    isComplete: (d) => Object.keys(d.locations).length > 0,
    errors: none,
    Component: LocationsStep,
  },
  {
    id: "provider",
    title: "Provider",
    isComplete: (d) => !!d.providers[0]?.type,
    errors: none,
    Component: ProviderStep,
  },
  {
    id: "models",
    title: "Models",
    isComplete: (d) =>
      (d.providers[0]?.models?.length ?? 0) > 0 &&
      validateModels(d).length === 0,
    errors: validateModels,
    Component: ModelsStep,
  },
  {
    id: "persona",
    title: "Persona",
    isComplete: (d) => !!d.persona.mode,
    errors: none,
    Component: PersonaStep,
  },
  {
    id: "services",
    title: "Services",
    optional: true,
    isComplete: (d) => Object.keys(d.services).length > 0,
    errors: validateServices,
    Component: ServicesStep,
  },
  {
    id: "access",
    title: "Access",
    optional: true,
    isComplete: (d) => d.access.mode === "lan" && validateAccess(d).length === 0,
    errors: validateAccess,
    Component: AccessStep,
  },
  {
    id: "boot",
    title: "Start on boot",
    optional: true,
    isComplete: () => false,
    errors: none,
    Component: StartOnBootStep,
  },
  {
    id: "multi-agent",
    title: "Multi-agent",
    optional: true,
    isComplete: (d) =>
      d.pipeline.include === true && validateMultiAgent(d).length === 0,
    errors: validateMultiAgent,
    Component: MultiAgentStep,
  },
  {
    id: "review",
    title: "Review",
    // "Complete" here means writable: something to write, nothing invalid.
    isComplete: (d) => d.providers.length > 0 && validateDraft(d).length === 0,
    errors: validateDraft,
    Component: ReviewStep,
  },
];

/** Index of the review step — the "skip to review" jump target. */
export const REVIEW_INDEX = STEPS.length - 1;
