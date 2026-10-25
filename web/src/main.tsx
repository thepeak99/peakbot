import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "./App";
import { Boot } from "./Boot";
import { isDesktop } from "./desktop";
import { bootDesktopTabs } from "./useConvoTabs";

// Desktop app only: restore the conversation tabs (and point the URL at the
// active one) before `useAgent` reads it on first render. `null` in a browser.
const initialTabs = isDesktop() ? bootDesktopTabs() : null;

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {/* T33 (D10/D12): Boot owns the first-run gate — on an unconfigured
        server it renders the guided setup (and the `/setup` alias) instead
        of mounting App (whose socket would 503). */}
    <Boot>
      <App initialTabs={initialTabs} />
    </Boot>
  </StrictMode>,
);
