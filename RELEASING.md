# Releasing clauhist

A release touches **two places**, and both must be updated:

- **crates.io** — `cargo publish` uploads the crate.
- **GitHub Releases** — a git tag plus a Release, which is the changelog users
  see.

`cargo publish` does **not** create a tag or a GitHub Release. Publishing without
the steps below is the usual reason a version appears on crates.io but not on the
GitHub Releases page.

## Checklist

1. **Start from an up-to-date, clean `main`.**
   ```sh
   git switch main
   git pull --ff-only
   git status            # must be clean
   ```

2. **Bump the version** in `Cargo.toml`, then refresh the lockfile:
   ```sh
   cargo build           # updates Cargo.lock to the new version
   ```
   Follow semver: a new shell or flag is a minor bump; a breaking change to a
   user-visible interface is a major bump.

3. **Check the release locally.**
   ```sh
   cargo fmt --check
   cargo test
   cargo clippy --all-targets
   ```

4. **Commit and push the bump.**
   ```sh
   git add Cargo.toml Cargo.lock
   git commit -m "Bump version to X.Y.Z"
   git push origin main
   ```

5. **Publish to crates.io.**
   ```sh
   cargo publish --dry-run   # packages and verifies the build
   cargo publish
   ```
   Published versions cannot be deleted, only yanked — double-check the version
   number before running `cargo publish` for real.

6. **Create the GitHub Release with the same version.** This creates and pushes
   the tag, and drafts notes from the merged PRs since the last release:
   ```sh
   gh release create vX.Y.Z --target main --title "vX.Y.Z" --generate-notes
   ```
   Review the notes: the generator can lag or use stale PR titles, so edit them
   if needed.
   ```sh
   gh release edit vX.Y.Z --notes-file <file>
   ```

7. **Verify both sides match.**
   ```sh
   cargo search clauhist          # crates.io latest version
   gh release list                # GitHub latest release
   ```

## Optional: automate it

A GitHub Actions workflow that runs `cargo publish` and `gh release create` when
a `v*` tag is pushed would remove the manual crates.io/GitHub split. Not set up
yet.
