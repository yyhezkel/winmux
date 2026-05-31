## What this changes

(One-line description of the user-visible change, or the developer-facing change if it's internal.)

## Why

(The motivation. If this fixes an issue, link it: `Fixes #N`.)

## How to test

(Steps a reviewer should follow to verify the change works.)

## Checklist

- [ ] Cargo check + tests pass (`cargo check && cargo test`)
- [ ] TypeScript compiles (`npx tsc --noEmit` from `app/`)
- [ ] Any new user-visible strings have i18n keys in all 4 locales (en/he/ar/ru)
- [ ] If touching docs: CLAUDE.md, DECISIONS.md, or the README updated as appropriate
- [ ] Commit message follows `Phase <N.M>: short summary` or `chore/docs/fix: ...` (see docs/CONTRIBUTING.md)
- [ ] No PTY input/output content in logs (CLAUDE.md Rule 1)
- [ ] No plaintext secrets at rest (CLAUDE.md Rule 2)
