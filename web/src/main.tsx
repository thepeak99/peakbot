import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "./App";
import { Setup } from "./setup/Setup";
import { isDesktop } from "./desktop";
import { bootDesktopTabs } from "./useConvoTabs";

// Two entry points, no router: `/setup` is the configuration wizard, everything
// else is the chat. The axum fallback already serves index.html for unknown
// paths, so this one branch is the whole routing story (plan §8.2) — and it
// keeps the wizard clear of `useAgent`, which the chat mounts.
const isSetup = window.location.pathname.startsWith("/setup");

// Desktop app only: restore the conversation tabs (and point the URL at the
// active one) before `useAgent` reads it on first render. `null` in a browser.
const initialTabs = !isSetup && isDesktop() ? bootDesktopTabs() : null;

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {isSetup ? <Setup /> : <App initialTabs={initialTabs} />}
  </StrictMode>,
);
