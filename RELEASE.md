# Publishing a release

Downloads live in [GitHub Releases](https://github.com/simpsoka/zigzag/releases).
The README uses a stable `releases/latest/download/ZigZag.dmg` link for the
Mac download; every release includes that filename, a versioned DMG, and
`SHA256SUMS.txt`. The Windows zip is attached to the same release by a
GitHub Actions workflow — one release page, one tag, two platforms.
Build products stay in the ignored `dist/` directory, outside Git history.

## Mac

### One-time setup

Install Rust, Xcode command line tools, and the [GitHub CLI](https://cli.github.com/).
Run `gh auth login` for an account with write access to `simpsoka/zigzag`.
Set up your Developer ID certificate and the `zigzag-notary` keychain profile
as described in the README's **Mac app** section. Signing credentials stay in
your Mac's keychain.

### Publish

1. Update `version` in `Cargo.toml` and regenerate `Cargo.lock` with Cargo.
   If the lockfile changes, regenerate `THIRD-PARTY-NOTICES.md` with
   `packaging/licenses/generate.sh`.
2. Add `packaging/releases/<version>.md` with the release notes and update
   `CHANGELOG.md`.
3. Test the app, commit all changes, and push `main` to GitHub.
4. Run:

   ```sh
   bash packaging/macos/publish-release.sh
   ```

The command builds a universal, signed, notarized DMG, verifies it, tags the
pushed commit, uploads the assets to a draft release, then publishes it as
the latest release. It requires a clean checkout on `main` with `origin`
pointing to this repository. It never replaces an existing release or tag.

To publish a DMG already built from the current commit:

```sh
bash packaging/macos/publish-release.sh --existing
```

Only use `--existing` when `dist/ZigZag-<version>.dmg` was built from the
current source. It verifies the image and Apple's notarization ticket, but
cannot establish which source commit produced an existing binary.

If an upload fails, the release stays a draft. Finish uploading the two DMGs
and `SHA256SUMS.txt` in GitHub's release editor before publishing it. If only
the tag was pushed, create the release from that tag in GitHub. The script
deliberately refuses to overwrite either.

## Windows

The Windows zip is built by `.github/workflows/windows-release.yml` on GitHub
Actions. Nothing needs to run on a Windows machine locally.

The workflow fires once the GitHub release is **published** (the Mac flow
above flips the draft to published on its last step), builds
`ZigZag-<version>-x86_64-windows.zip` on the `windows-latest` runner, and
uploads it alongside the DMGs on the same release — plus a
`SHA256SUMS-windows.txt` file. No signing: Windows SmartScreen warns on
first run until the app builds reputation (click *More info* → *Run anyway*).

The zip bundles `ZigZag.exe` (the studio), `zigzag-cli.exe`, `zigzag-mcp.exe`,
`LICENSE`, `NOTICE`, `THIRD-PARTY-NOTICES.md`, and a short `README.txt`.
The binaries statically link the MSVC runtime, so users don't need the
Visual C++ redistributable.

### Rebuild without re-tagging

Use the workflow's `workflow_dispatch` entry from the Actions tab: enter the
existing tag (for example `v0.1.0`) and run it. The zip and checksum file on
the release are replaced in place (`gh release upload --clobber`). Use this
to retry a failed build, or to produce a Windows zip for a Mac release that
shipped without one.
Every run checks out the requested tag, including manual retries, so the
Windows and Mac binaries come from the same source.

### If the Mac flow never publishes the release

Dispatch the Windows workflow manually with the tag. It creates the release
as a draft if it isn't there yet, uploads the zip, and leaves it draft so
the Mac flow can finish it later (or so you can publish it by hand).

### Regenerate the Windows icon

The committed `.ico` is a snapshot of `packaging/macos/AppIcon.png` at the
sizes Windows uses. To regenerate it after changing the PNG:

```sh
bash packaging/windows/make-icon.sh    # needs Python + Pillow
```

The CI workflow regenerates the `.ico` every build too, so a stale commit is
harmless as long as the source PNG is up to date.

## Verify a download

Download a DMG and `SHA256SUMS.txt` from the same release, then run:

```sh
shasum -a 256 --ignore-missing -c SHA256SUMS.txt
```

The two DMG filenames contain identical bytes; either can be used to install.

For the Windows zip:

```sh
shasum -a 256 --ignore-missing -c SHA256SUMS-windows.txt   # on Mac or Linux
certutil -hashfile ZigZag-<version>-x86_64-windows.zip SHA256   # on Windows
```
