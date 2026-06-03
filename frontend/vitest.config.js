import { defineConfig } from 'vitest/config';

// Tests live here; the modules under test are the raw served assets in
// ../coordinator/src/http/static (imported by relative path). jsdom gives us a
// DOM + events so we can exercise gesture/rendering helpers headlessly.
export default defineConfig({
  test: {
    environment: 'jsdom',
    include: ['**/*.test.js'],
  },
});
