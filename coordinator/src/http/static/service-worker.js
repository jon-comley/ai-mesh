// Network-first service worker. The previous version was cache-first and served
// '/', style.css and dashboard.js from cache forever — so a coordinator deploy
// never reached the browser (the SW bypasses the server's Cache-Control). Now we
// always hit the live coordinator and only fall back to cache when offline, so
// deploys take effect on the next load. Bump CACHE to purge any stale entries.
const CACHE = 'mesh-v5';

self.addEventListener('install', e => {
  e.waitUntil(caches.open(CACHE).then(c => c.add('/')));
  self.skipWaiting();
});

self.addEventListener('activate', e => {
  e.waitUntil(
    caches.keys()
      .then(keys => Promise.all(keys.filter(k => k !== CACHE).map(k => caches.delete(k))))
      .then(() => clients.claim())
  );
});

self.addEventListener('fetch', e => {
  const url = e.request.url;
  // Never intercept WebSocket upgrades or API calls.
  if (url.includes('/ws') || url.includes('/api/')) return;

  // Network-first: always try the coordinator so a deploy is picked up
  // immediately; only fall back to cache when the network fails (offline).
  e.respondWith(
    fetch(e.request)
      .then(res => {
        // Keep the app shell cached for offline use.
        if (e.request.mode === 'navigate') {
          const copy = res.clone();
          caches.open(CACHE).then(c => c.put('/', copy));
        }
        return res;
      })
      .catch(() =>
        caches.match(e.request).then(
          cached => cached || (e.request.mode === 'navigate' ? caches.match('/') : undefined)
        )
      )
  );
});
