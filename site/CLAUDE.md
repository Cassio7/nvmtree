# site/ (landing page)

The nvmtree landing page. The CLI's own context lives in the repo root `CLAUDE.md`
and `README.md`: take facts (commands, flags, install lines, version) from there,
never invent them.

## Constraints

- One static `index.html`, no framework, no bundler, no npm dependencies. Fonts from
  Google Fonts; everything else local in `assets/`.
- Brand: black and white only (`#fafafa` ground, `#111111` ink, no accent colour),
  terminal face (Geist Mono) for headlines and the wordmark, Geist for text, the
  ASCII tree output as the main picture. Radius `sharp`.
- Terminal windows show real `nvmtree` output (run it against a fake `NVM_DIR` to
  refresh them; see the root CLAUDE.md for the nvm layout). Keep the package names
  generic (typescript, pnpm, eslint, @shopify/cli), never the owner's real setup.
- The one main action is "Download nvmtree" (→ `#install`), worded the same in the
  nav, the hero and the close. The install commands must match the root README.
- When the CLI version changes, update the hero's version link (`.hero__note`) and the
  nav menu card (`nvmtree --version`).

## Design skill

Built with `design-taste-frontend` (in `.claude/skills/`, installed with
`npx skills add Leonxlnx/taste-skill`). Its rules: one section per screen (hero
alone on the first screen, every section exactly 1 or 2 screens at 1440x900), equal
left/right edges on the `.wrap`, plain copy (headings name things, buttons are a verb
and an object), no captions under visuals, no invented proof.

- `assets/` is the skill's kit, copied: never edit it to make a check pass. Page CSS
  goes in the `<style>` block in `index.html`, only inside visuals.
- The deal (hero `triptych`, `nav-mega`, `close-word`, `footer-columns`, rhythm
  `space`, buttons `arrow`, mark `stack`, effect `gradient-mesh` in the install frame,
  card grounds `painterly-landscape` held still in greys) is recorded in the comment at
  the top of `<body>`, with the section plan; keep it unless redoing the design.
- Sections: hero (three sets of terminal pieces that rotate) · `#updates` (two-pane
  terminal: `--table -u` and `-u`) · `#commands` (2x2 cards over two screens, `.g2`) ·
  `#install` · `#questions` · `footer.closing`.
- The checks in `README.md` are a guide, not a gate: run them once at most for a
  big change, fix what is visibly wrong (cut text, overlaps, broken layout), and
  ignore spacing statistics (`section-gaps`, `section-air`, a few px of
  `section-height`). Never loop on the checker; ask the owner instead.

## Commits

Follow the root conventions: `feat(site): ...`, `fix(site): ...`, `docs(site): ...`.
