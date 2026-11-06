# nvmtree landing page

The project's landing page: one static `index.html`, no framework, no build step.
Meant for GitHub Pages.

## Files

- `index.html`: the page (markup, page CSS and the small copy-button script).
- `assets/`: the CSS and JS it links, copied from the design skill's kit
  (`system.css`, `system.js`, `motion/`, `layout/`, `ui/`, `effects/fluted-glass`, ...).
  Don't edit these by hand; recopy them from the skill if it's updated.
- `.claude/skills/design-taste-frontend/`: the design skill used to build the page
  (`npx skills add Leonxlnx/taste-skill`, pinned in `skills-lock.json`).
- `.taste-skill/`: the skill's deal history (palettes, heroes, effects already used).

## Preview locally

```bash
cd site && python3 -m http.server 8000   # then open http://localhost:8000
```

Use a local server, not `file://`: the scripts are ES modules and won't load from a file URL.

## Publish on GitHub Pages

`.github/workflows/pages.yml` deploys `index.html` and `assets/` on every push to `main`
that touches `site/` (or by hand from the Actions tab). One-time setup: Settings → Pages →
Build and deployment → Source: "GitHub Actions".

The page is at `https://cassio7.github.io/nvmtree/`.

## Checks

The skill's checkers need Chrome or a Chromium browser (`CHROME_PATH` for Brave/Edge):

```bash
S=.claude/skills/design-taste-frontend
node $S/scripts/consistency.mjs index.html --widths 1440,1024,820,768,390 --rhythm-widths 1440,820,768,390 \
  --profile /tmp/nvmtree-chrome --page software --category "a CLI that lists global npm packages per Node version" --primary "Download nvmtree"
node $S/assets/layout/check.mjs --url index.html --profile /tmp/nvmtree-chrome --sizes 1440x900,1024x768,820x1180,768x1024,390x844
node $S/scripts/perf.mjs index.html --profile /tmp/nvmtree-chrome
```
