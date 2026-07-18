// Notification-only service worker. Its sole reason to exist is to make
// task-completion notifications work on mobile browsers, where the
// `new Notification()` constructor throws `Illegal constructor` and
// `ServiceWorkerRegistration.showNotification()` is the only path (see
// Chromium bug 481856 / MDN Notification constructor notes).
//
// Deliberately has NO `fetch` handler — it must never cache or intercept
// requests, so the WS app and its assets are unaffected. This is not a PWA.

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

// Focus an existing tab (or open one) when a notification is tapped.
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((clients) => {
      for (const client of clients) {
        if ("focus" in client) return client.focus();
      }
      if (self.clients.openWindow) return self.clients.openWindow("/");
    }),
  );
});
