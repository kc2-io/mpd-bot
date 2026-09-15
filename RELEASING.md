# Releasing MPD Bot

## Ship a release

1. Commit the finished source, workflow and lockfile changes, push them, and choose the commit to ship. For a normal release, use the reviewed `main` commit after CI passes.
2. Create an annotated semantic-version tag and push **that tag**:

   ```powershell
   git switch main
   git pull --ff-only
   git tag -a v0.2.0 -m "MPD Bot 0.2.0"
   git push origin v0.2.0
   ```

   Replace `0.2.0` with the version being shipped. For a prerelease use, for example, `v0.2.0-rc.1`. Do not reuse or move a published version tag. Bumping Cargo.toml is not required: release tags are the published version source.
3. Open the repository's **Actions → Release** run. It builds the tagged commit, runs Windows formatting/tests/Clippy, verifies the executable's version, packages it, and publishes a GitHub release after its assets are uploaded.
4. Download the ZIP from the new release page, extract it, launch `mpd-bot.exe`, and check **About → Version**. Quit the previous app before upgrading. User settings and credentials remain in the user's configuration directory.

A local tag alone does not start GitHub Actions. The tag push triggers the workflow; the tagged commit must include `.github/workflows/release.yml`. See GitHub's [tag-filter workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#onpushpull_requestpull_request_targetbranchesbranches-ignoretags-tags-ignore).

## Outputs and versioning

For `v0.2.0`, the release is titled **MPD Bot v0.2.0** and includes:

- `mpd-bot-v0.2.0-windows-x86_64.zip`, containing only `mpd-bot.exe` and a short `README.txt`.
- `SHA256SUMS.txt`, the SHA-256 checksum of the ZIP.
- Automatically generated GitHub release notes.

Tags with a semantic-version prerelease suffix (for example `-rc.1`) produce a GitHub prerelease. Build metadata alone (for example `+build.7`) does not mark a release as a prerelease.

The workflow passes the tag version, without `v`, as `MPD_BOT_RELEASE_VERSION`. `build.rs` validates it as SemVer and embeds it at compile time. The About tab, `--version` output, and crash marker all use that same value. Without the override, local development builds use the package version from Cargo.toml. Changing a runtime environment variable does not alter an already-built executable.

The release target is currently **Windows x64 (`x86_64-pc-windows-msvc`)**. Release artifacts statically link the MSVC runtime and embed the UI/logo. There is no installer, executable signing, or auto-updater in this workflow. Existing macOS/Linux CI checks remain separate; only Windows is published as a release asset. Repository visibility controls who can access the release downloads.

## Failure and retry behavior

- Invalid tags, failing checks, mismatched embedded versions, or checksum failures stop publication.
- The build job has read-only repository access. Only the publish job receives `contents: write` through the built-in GitHub Actions token; no personal token is needed.
- The publish job verifies that the tag still identifies the built commit. Do not move tags while a release is building.
- A release is created as a draft, its assets are uploaded, and then it is published. A failed upload leaves a draft that a workflow rerun can resume.
- Rerunning after publication leaves the existing release assets unchanged. Publish a new version for different binaries.
- If source changes are required after a failed run, commit them and use a new version tag. Rerunning the original tag builds the original tagged source.

The draft/upload/publish steps use the official [GitHub release CLI](https://cli.github.com/manual/gh_release_create), including [draft publication](https://cli.github.com/manual/gh_release_edit). GitHub Actions must be enabled and organization policy must permit release creation using the workflow token.

## Local packaging check (PowerShell 7)

This builds and packages locally without creating a tag or publishing anything:

```powershell
$env:MPD_BOT_RELEASE_VERSION = '0.2.0-rc.1'
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --locked --target x86_64-pc-windows-msvc
./scripts/package-windows.ps1 -Version '0.2.0-rc.1' -OutputDirectory '.test-data/release-check'
Remove-Item Env:MPD_BOT_RELEASE_VERSION
Remove-Item Env:RUSTFLAGS
```

Use a fresh output directory each time. Packaging runs `--version` with redirected output and refuses an executable with a different embedded version. It packages only an explicit file allowlist; local config, logs, API keys and Twitch tokens are never included. The resulting ZIP checksum can be checked with `Get-FileHash -Algorithm SHA256` against `SHA256SUMS.txt`.
