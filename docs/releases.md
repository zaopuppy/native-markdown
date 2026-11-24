# Automated Windows releases

The `Release` GitHub Actions workflow publishes Windows x64 portable ZIPs.
The existing `Rust` workflow continues to build and test ordinary pushes and PRs.

## Publish a version

1. Change `[package].version` in `Cargo.toml`, for example from `0.1.0` to `0.1.1`.
2. Run `cargo check` to update the root package version in `Cargo.lock`.
3. Commit both files together with the changes being released, and push to `master`.
4. Watch **Actions → Release**. After tests, a release build and packaging succeed,
   the workflow creates tag `v0.1.1` at that push's exact commit and publishes a Release.

Detection compares the version at the push's `before` commit with its final commit,
including pushes containing several commits. Changing dependencies or other manifest
fields without changing the package version does not trigger a release build.
The workflow configuration alone does not release the current version.

The Release contains:

- `native-markdown-v0.1.1-windows-x64.zip`
- `native-markdown-v0.1.1-windows-x64.zip.sha256`
- Automatically generated GitHub release notes.

The ZIP includes the executable, app-local Visual C++ runtime DLLs, English/Chinese
usage documentation, the sample document, bundled web licenses, and license metadata
and notice files shipped in resolved Rust dependencies. Web assets are embedded in
the executable; Node and a frontend build step are not required. The system
Microsoft Edge WebView2 Runtime remains a prerequisite and is not bundled.

Extract the entire ZIP before starting `native-markdown.exe`. No installer is needed.
Settings and history still use `%APPDATA%/NativeMarkdown`, as described in the README.

To verify a download in PowerShell:

```powershell
Get-FileHash .\native-markdown-v0.1.1-windows-x64.zip -Algorithm SHA256
Get-Content .\native-markdown-v0.1.1-windows-x64.zip.sha256
```

## Retry or publish the current version

- For a failed run, use **Re-run failed jobs** or **Re-run all jobs** on that run.
- To publish the current version without another version bump (including the first
  `0.1.0` release), use **Actions → Release → Run workflow**, selecting `master`.
  Manual dispatch on other branches is rejected.
- Uploads go to a draft; only complete uploads become public. Failed uploads leave
  a draft which the same commit can resume. Already public releases are not changed.
- A version tag belonging to a different commit causes a failure. Bump the version
  for new code rather than moving a tag or replacing a published download.
- Versions such as `0.2.0-beta.1` become prereleases and do not replace Latest.
- Creating a new `master` branch has no comparison baseline and does not publish
  automatically; use manual dispatch if needed.

## Repository setup and validation

Enable GitHub Actions and allow the official `actions/*` actions. Publication uses
the built-in `GITHUB_TOKEN`; only the publish job requests `contents: write`. No
personal token or local GitHub login is needed by the workflow. Organization rules
or tag protection rules must allow this workflow to create release tags and Releases.

Intermediate Actions artifacts are retained for 14 days; public downloads are
attached to the Release. This pipeline does not sign the executable or installer.

Local release automation tests (Python 3.11+):

```powershell
python -B -m unittest discover -s tests -p test_release.py
```

These tests validate version gates, packaging, checksums and mocked publication
recovery. They do not replace a successful hosted Actions build or a launch on a
clean Windows machine with WebView2 installed.
