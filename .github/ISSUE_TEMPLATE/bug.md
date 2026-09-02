---
name: 🐛 Bug report
about: Something is broken or behaves wrong
title: "bug: <short description>"
labels: ["bug"]
---

## What happened

<!-- One paragraph: the symptom, in the user's voice. -->

## What I expected

<!-- What should have happened instead. -->

## Steps to reproduce

1.
2.
3.

## Environment

- OS / version:
- Luna Agent version: (e.g. `1.0.0` from `package.json`, or commit SHA)
- Build type: dev (`npm run tauri:dev`) / release (`npm run tauri:build`)
- API provider(s) in use: Anthropic / MiniMax / both
- Workspace: empty folder / small project / large project / (not applicable)

## Logs / screenshots

<!-- Paste console output, or `tracing` output from the Rust side.
     The webview DevTools console + the terminal running `tauri:dev` are
     the most useful places. Strip API keys before pasting! -->

```
<paste here>
```

## Severity

- [ ] Blocker (cannot use the app at all)
- [ ] Major (core feature broken, no workaround)
- [ ] Minor (annoying but functional)
- [ ] Cosmetic (looks wrong, doesn't affect function)

## Possible cause / hint (optional)

<!-- If you have a hunch — file / line / Tauri command — drop it here.
     Not required, but speeds up triage. -->

## Acceptance criteria for the fix

<!-- What would you accept as "fixed"? One or two sentences. -->
