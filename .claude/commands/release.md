---
allowed-tools: Bash, Edit, Read, Glob
argument-hint: [version] (e.g., 0.0.2)
description: Automated release process - version bump, changelog, tag, homebrew
---

# Release Process for ptyZZZ

## Pre-flight Checks

Current branch: !`git branch --show-current`

Last releases: !`git tag --sort=-version:refname | head -5`

Current version: !`grep '^version' Cargo.toml | head -1`

## Steps

### 1. Version Bump

- Update version in `Cargo.toml` to $ARGUMENTS
- Run `cargo check` to update `Cargo.lock`

### 2. Generate Changelog

Get commits since the last release:

```bash
last_tag=$(git tag --sort=-version:refname | head -1)
git log --oneline --pretty=format:"* %s (%ad)" --date=short ${last_tag}..HEAD
```

Create `changes/v$ARGUMENTS.md` with:

- `# v$ARGUMENTS` header
- `## Highlights` section with notable user-facing changes
- `## Raw commits` section with commit list
- **No soft line breaks** -- paragraphs should be single long lines, not wrapped at 80 columns. GitHub renders markdown with soft wraps, so hard breaks mid-paragraph show up as unwanted newlines in the release notes.

### 3. Review

**REVIEW REQUIRED**: Show the changelog for user approval before proceeding.

### 4. Commit and Tag

```bash
git add Cargo.toml Cargo.lock changes/v$ARGUMENTS.md
git commit -m "chore: release v$ARGUMENTS"
git tag v$ARGUMENTS
```

### 5. Push

```bash
git push && git push --tags
```

This triggers the GitHub workflow (cablehead/pipelines release-binaries) to build cross-platform binaries.

### 6. Monitor Build

```bash
gh run list --limit 1
gh run watch <run-id> --exit-status
```

### 7. Homebrew Formula Update

- Clone `../homebrew-tap` if not present:
  `git clone https://github.com/cablehead/homebrew-tap.git`
- **Pull latest** before making changes: `cd ../homebrew-tap && git pull`
- **Wait 10+ seconds** after build completes for GitHub CDN propagation
- Download the macOS tarball, verify integrity, and calculate SHA256:
  ```bash
  cd /tmp
  rm -f ptyZZZ-v$ARGUMENTS-darwin-arm64.tar.gz
  curl -sL https://github.com/cablehead/ptyZZZ/releases/download/v$ARGUMENTS/ptyZZZ-v$ARGUMENTS-darwin-arm64.tar.gz -o ptyZZZ-v$ARGUMENTS-darwin-arm64.tar.gz
  tar -tzf ptyZZZ-v$ARGUMENTS-darwin-arm64.tar.gz
  sha256sum ptyZZZ-v$ARGUMENTS-darwin-arm64.tar.gz
  ```
- Update `../homebrew-tap/Formula/ptyzzz.rb` with the new version, URL, and SHA256 checksum
- Commit and push the homebrew formula changes

### 8. Manual Verification

Ask a macOS user to test:

```bash
brew uninstall ptyzzz  # if previously installed
brew install cablehead/tap/ptyzzz
ptyZZZ --help
```

Note: no crates.io publish. ptyZZZ pins wezterm-term to a git rev, and crates.io rejects git dependencies (see docs/adr/0001; revisit if the rio-vt branch is promoted, since rio-vt is a published crate).

## Release Complete
