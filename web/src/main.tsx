import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "./App";
import { Setup } from "./setup/Setup";

// Two entry points, no router: `/setup` is the configuration wizard, everything
// else is the chat. The axum fallback already serves index.html for unknown
// paths, so this one branch is the whole routing story (plan §8.2) — and it
// keeps the wizard clear of `useAgent`, which the chat mounts.
const isSetup = window.location.pathname.startsWith("/setup");

createRoot(document.getElementById("root")!).render(
  <StrictMode>{isSetup ? <Setup /> : <App />}</StrictMode>,
);
