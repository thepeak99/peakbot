import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "./App";
import { isDesktop } from "./desktop";
import { bootDesktopTabs } from "./useConvoTabs";

// Desktop app only: restore the conversation tabs (and point the URL at the
// active one) before `useAgent` reads it on first render. `null` in a browser.
const initialTabs = isDesktop() ? bootDesktopTabs() : null;

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App initialTabs={initialTabs} />
  </StrictMode>,
);
