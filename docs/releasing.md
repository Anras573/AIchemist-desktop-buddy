# Releasing

Releases are built by [`.github/workflows/release.yml`](../.github/workflows/release.yml) when a
`vX.Y.Z` tag is pushed. The workflow builds **unsigned** installers (spec sections 2 and 14.7: no
code signing, no notarization), writes `SHA256SUMS.txt` and creates a **draft** GitHub release. A
maintainer reviews the draft and publishes it by hand. Nothing is published automatically.

## Versioning

- Plain [SemVer](https://semver.org/) `MAJOR.MINOR.PATCH`, no pre-release suffix (the Windows MSI
  bundler only accepts numeric versions). While the app is `0.x`, bump the minor version for new
  features and the patch version for fixes.
- The tag is the version with a `v` prefix: version `0.2.0` is tag `v0.2.0`. The update check
  (task 4.1) compares against the `tag_name` of the latest published release, so the format
  matters.
- The version lives in five places, which must agree:

  | File | Field |
  |---|---|
  | `package.json` | `version` |
  | `package-lock.json` | `version` and `packages[""].version` |
  | `src-tauri/Cargo.toml` | `[package] version` |
  | `src-tauri/Cargo.lock` | the `aichemist-desktop-buddy` entry |
  | `src-tauri/tauri.conf.json` | `version` (used for installer names and bundle metadata) |

  `tools/bump-version.mjs` updates all of them; do not edit them by hand.

## Cutting a release

1. **Bump the version** on a branch and open a PR:

   ```sh
   git switch -c release/v0.2.0 origin/main
   node tools/bump-version.mjs 0.2.0
   node tools/bump-version.mjs --check      # all files agree
   git commit -am "chore: release 0.2.0"
   git push -u origin release/v0.2.0
   gh pr create --fill
   ```

   Merge it once CI is green.

2. **Tag the merge commit on `main`** and push the tag:

   ```sh
   git switch main && git pull
   git tag -a v0.2.0 -m "AIchemist Desktop Buddy 0.2.0"
   git push origin v0.2.0
   ```

3. **Watch the Release workflow** (`gh run watch`, or the Actions tab). It:
   1. checks that the tag equals `v` + the version in all five files, and fails before building
      anything if not;
   2. builds Windows (NSIS `-setup.exe` and `.msi`, x64) and macOS (one universal `.dmg` for Apple
      silicon and Intel) installers. Linux (`.deb`, `.AppImage`) is best-effort: if it fails,
      the release goes ahead without it;
   3. writes `SHA256SUMS.txt` over all installers and renders the notes from
      [`.github/release-notes.md`](../.github/release-notes.md) (unsigned-build warning, link to the
      README install section, download table);
   4. creates a draft release named `AIchemist Desktop Buddy X.Y.Z` with the installers, the
      checksums and the notes, followed by GitHub's generated list of merged PRs.

4. **Review the draft** on the Releases page: download the Windows and macOS installers, check
   them against `SHA256SUMS.txt`, install and start them once, and add a short highlights
   paragraph to the notes. Then select **Publish release**.

Publishing is what makes the release visible to users and to the in-app update check (drafts
are not returned by the "latest release" API).

### If something goes wrong

- **Tag does not match the version:** delete the tag (`git push origin :refs/tags/v0.2.0` and
  `git tag -d v0.2.0`), fix the version through a PR, and tag again.
- **A build failed for a transient reason:** re-run the failed jobs. If the draft already
  exists, the publish job replaces its files (`--clobber`). It refuses to touch a release that is
  already published.
- **A bad release was published:** do not move or reuse its tag. Fix forward with a new patch
  version, and mark the bad release as a pre-release or delete it.

## Dry runs

The same build and packaging run without publishing:

- on any PR that changes `release.yml`, `.github/release-notes.md` or `tools/bump-version.mjs`;
- by hand: **Actions > Release > Run workflow**, or
  `gh workflow run release.yml --ref <branch>` (the workflow must exist on `main` for GitHub to
  accept a manual run).

A dry run uploads the installers as `bundle-<os>` artifacts and the full would-be release
(installers, `SHA256SUMS.txt`, `notes.md`) as the `release-draft` artifact, kept for 14 days.

## Workflow design notes

- **No `tauri-apps/tauri-action`.** The workflow calls `npm run tauri build` directly. The Tauri
  CLI version is then pinned by `package-lock.json`, like in CI, and no third-party action ever
  holds a token that can write to the repository. The action's extras (uploading to a release,
  updater JSON) are not needed: we have no updater, and uploading is one `gh release` call.
- **Only GitHub-owned actions, pinned to commit SHAs** (`checkout`, `setup-node`,
  `upload-artifact`, `download-artifact`). Rust comes from the runner's preinstalled `rustup`.
  Update the pins deliberately (the tag is in a comment next to each SHA).
- **No build cache.** Release builds start clean, so a cache written by another branch cannot
  end up in a release. They take longer; releases are rare.
- **Least privilege.** The workflow's default token is `contents: read`. Only the `publish` job
  gets `contents: write`, and it runs no project code: it downloads the packaged files and calls
  `gh release`. No secrets besides `GITHUB_TOKEN`. No signing keys exist.
- **macOS universal binary.** One `.dmg` for both architectures, built with
  `--target universal-apple-darwin` (both Rust targets installed). Users do not have to pick.
- **Asset names have no spaces** (`AIchemist-Desktop-Buddy_0.2.0_x64-setup.exe`); the bundler's
  names contain the product name with spaces, which GitHub would rewrite to dots.
