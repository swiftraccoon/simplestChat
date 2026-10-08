/* This worker handles generic notifications only. It never caches requests or account data. */
self.addEventListener('install', (event) => {
  event.waitUntil(self.skipWaiting());
});
self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim());
});
self.addEventListener('push', (event) => {
  event.waitUntil(
    self.registration.showNotification('simplestChat', {
      body: 'New private messages',
      tag: 'private-messages',
      icon: '/icon-192.png',
      badge: '/icon-192.png',
    }),
  );
});
self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then(async (windows) => {
      const existing = windows.find((client) => {
        const url = new URL(client.url);
        return url.origin === self.location.origin && url.pathname === '/';
      });
      if (existing) {
        await existing.focus();
        existing.postMessage({ type: 'openMessages' });
      } else await self.clients.openWindow('/?messages=1');
    }),
  );
});
self.addEventListener('message', (event) => {
  if (event.data?.type !== 'clearNotifications') return;
  event.waitUntil(
    self.registration.getNotifications({ tag: 'private-messages' }).then((notifications) => {
      for (const notification of notifications) notification.close();
    }),
  );
});
