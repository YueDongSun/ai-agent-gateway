import { defineConfig } from 'vitest/config';

// Vitest config — scope test discovery to the repo's own tests/ (and src/ for
// any inline `*.test.ts` files). The gitignored reference checkout at
// `cc-switch/` contains its own test suite + `@/` path aliases that this repo
// does not configure; without this include glob vitest would pick those up and
// fail. See PROGRESS.md "Verification commands".
export default defineConfig({
  test: {
    include: ['tests/**/*.test.ts', 'src/**/*.test.ts'],
    environment: 'node',
  },
});
