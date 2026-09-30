---
name: build-release
description: Build a shareable Dev Pilot Board package (universal .app + notify.sh + installer, zipped under dist/). Use when asked to build, package, cut, or share a release, or to prepare a build to send to testers. Optionally tags, publishes the GitHub release and bumps the Homebrew cask.
---

# Build a shareable release

The whole build/package step is one script. Run it, don't reimplement it:

```bash
scripts/build-release.sh            # universal release build (default for sharing)
scripts/build-release.sh --debug    # quick host-arch build for local testing
scripts/build-release.sh --skip-build   # repackage the last build only
```

It checks the three version files agree, builds, verifies `lipo -archs` and the bundle
version, then writes `dist/dev-pilot-board-<v>/` and `dist/dev-pilot-board-<v>.zip` and
prints the zip path plus its sha256. A universal build takes several minutes — run it in
the background with a 10 minute timeout.

## Before running

1. Decide the version. Any app or `notify.sh` behavior change since the last tag needs a
   bump in **all three**: `app/src-tauri/tauri.conf.json`, `app/package.json`,
   `app/src-tauri/Cargo.toml`. Packaging/docs-only fixes may reuse the version and
   replace the asset (`gh release upload --clobber`). Check `git tag` and `git diff --stat`.
2. The script kills a running Dev Pilot Board (the bundler fails otherwise). Say so.

## After the script succeeds

Report only: zip path, sha256, and how to verify (`unzip -l` or `open dist/...`).
The zip is what gets shared. Testers run `install.sh` from inside it; `INSTALL.md` covers
the quarantine step.

## Publishing (only when asked to release / publish / tag)

```bash
git add -A && git commit -m "v<v>: <summary>"
git tag v<v> && git push && git push --tags
gh release create v<v> "$PWD/dist/dev-pilot-board-<v>.zip" --title "Dev Pilot Board <v> — <summary>" --notes "<notes>"
```

Use an absolute asset path — a relative one has failed before. Then in the
`imyuvii/homebrew-tap` repo update `Casks/dev-pilot-board.rb`: `version`, `sha256`
(from the script output), commit and push. The folder name inside the zip
(`dev-pilot-board-<v>`) must match the cask's `app` stanza path.

## Failure modes

- `version mismatch` — bump the lagging file(s), rerun.
- `expected universal binary` — `rustup target add x86_64-apple-darwin aarch64-apple-darwin`.
- Missing `cargo` — `source ~/.cargo/env`; the script does this itself if the file exists.
- DMG/bundle errors after "Built application" are harmless; the `.app` exists. Use
  `--skip-build` to package it.
