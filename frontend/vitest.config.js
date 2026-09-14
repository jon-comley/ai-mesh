import { defineConfig } from 'vitest/config';

// Tests live here; the modules under test are the raw served assets in
// ../coordinator/src/http/static (imported by relative path). jsdom gives us a
// DOM + events so we can exercise gesture/rendering helpers headlessly.
export default defineConfig({
  // The served modules import each other by the URL the coordinator serves them
  // at (`/static/api.js`), which is right in the browser and unresolvable here.
  // Point that prefix at the directory they actually live in so a module with
  // dependencies is testable at all — before this, only the dependency-free
  // ones (drag, colormath, solar) could be imported.
  resolve: {
    alias: [{ find: /^\/static\//, replacement: new URL('../coordinator/src/http/static/', import.meta.url).pathname }],
  },
  test: {
    environment: 'jsdom',
    include: ['**/*.test.js'],
  },
});
