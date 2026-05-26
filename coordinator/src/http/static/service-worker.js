const CACHE = 'mesh-v3';
const PRECACHE = [
  '/',
  '/static/style.css',
  '/static/dashboard.js',
  '/static/topology.js',
  '/manifest.json',
];

self.addEventListener('install', e => {
  e.waitUntil(caches.open(CACHE).then(c => c.addAll(PRECACHE)));
  self.skipWaiting();
});

self.addEventListener('activate', e => {
  e.waitUntil(
    caches.keys().then(keys =>
      Promise.all(keys.filter(k => k !== CACHE).map(k => caches.delete(k)))
    ).then(() => clients.claim())
  );
});

self.addEventListener('fetch', e => {
  // Never intercept WebSocket upgrades or API calls — let them reach the network.
  const url = e.request.url;
  if (url.includes('/ws') || url.includes('/api/')) return;

  e.respondWith(
    caches.match(e.request).then(cached => {
      if (cached) return cached;
      return fetch(e.request).catch(() => {
        // Coordinator unreachable — return cached shell if available.
        if (e.request.mode === 'navigate') return caches.match('/');
      });
    })
  );
});
