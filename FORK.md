# About this fork

Demon Cut is a modified version of **Concat**, the free video editor by Jareer and the Concat
contributors (https://github.com/jub0t/Concat). It was forked from Concat 0.2.7 at upstream commit
`2d3a43e` on **12 October 2026**, and the full upstream history is kept in this repository.

Concat's code is AGPL-3.0-or-later, and so is Demon Cut's. The copyright notices in the source files
are Concat's and stay as they are; files added or changed for Demon Cut are its own work under the
same licence. This file is the notice the AGPL (section 5a) asks a modified version to carry.

## What changed on 12 October 2026

The first change is a rebrand. The editor itself, its engine and its features are Concat's.

- **Name.** The app is called Demon Cut everywhere a person sees a name: the window, Settings, the
  installers, the menus, the 25 translations.
- **Logo.** A new mark, drawn by [`scripts/make-logo.py`](scripts/make-logo.py), which writes every
  icon the builds use. Concat's logo and artwork are gone from the tree.
- **Identity, so the two apps can sit side by side.**
  - App identifier `com.demonstudios.demoncut` (was `app.concat.editor`): the settings, models and
    logs folders, the macOS and iOS bundle id, the Android package.
  - Binaries `demoncut` and `demoncut-cli` (were `concat` and `concat-cli`); Linux packages, the
    `.desktop` file and the window class are `demoncut`.
  - New Windows installer and MSI upgrade codes.
  - Release files are named `DemonCut-<version>-...`.
  - New projects default to a `Demon Cut` folder on the Desktop (or `Documents`, `Movies` on phones).
- **Updates** come from this repository's releases (`DaftHD/Demon-Cut`), not Concat's.
- **Documentation.** A new README. Concat's translated READMEs, audits, CLA, code-signing policy,
  sponsor material and screenshots were removed, since they speak for the Concat project.

## What deliberately stayed the same

- **Crate and module names** (`concat`, `concat-core`, `concat_media`, …), effect ids
  (`concat.blur`, …), environment variables (`CONCAT_LOG`, …) and the project file format
  (`concat.json`). Nobody using the app sees them, and keeping them means fixes from upstream
  Concat merge without conflicts. It also means Demon Cut and Concat open each other's projects.
- **The model mirror.** Captions, voices and cutout models still download from the mirror on
  Concat's GitHub release (`jub0t/Concat`, `models-v1`), checked against the SHA-256 in
  [`models/manifest.toml`](models/manifest.toml). To host your own copy, run the "Mirror models"
  workflow here and change `REPO` in `src/crates/concat-host/src/models.rs` and
  `scripts/models.py`.
- **Comments that link to Concat issues** (`jub0t/Concat/issues/...`) still point there: they explain
  why code is the way it is.

## Pulling in upstream fixes

```bash
git remote add upstream https://github.com/jub0t/Concat.git   # once
git fetch upstream
git merge upstream/main
```

Conflicts will mostly be in user-visible text that says "Concat" upstream and "Demon Cut" here.
