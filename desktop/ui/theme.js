// Theme for the bundled pages (desktop-pane design §3): follow the
// PeakBot web UI's theme as reported over `theme_set` (read back with
// `theme_get`), falling back to the OS preference until it reports one.
// Loaded last in <head> (CSP `script-src 'self'`: no inline scripts).
(function () {
  const os = () => (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
  const apply = (t) => document.documentElement.classList.toggle("light", t === "light");
  apply(os());
  const core = window.__TAURI__ && window.__TAURI__.core;
  if (core) core.invoke("theme_get").then((t) => apply(t ?? os())).catch(() => {});
})();
