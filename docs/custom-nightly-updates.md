# Keeping a customized Windows build current

The official in-app updater replaces the installed executable with the official release. A
custom executable is therefore built with the `custom-build` Cargo feature, which disables that
updater and keeps it from silently removing local behavior changes.

From a clean checkout of the Serein customization branch, run:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\scripts\update-custom-nightly.ps1
```

The script finds the newest published nightly release, merges its tag into the current branch,
runs `cargo xtask check`, builds with the repository's release profile (fat LTO), and replaces
`%LOCALAPPDATA%\Programs\Serein\serein.exe`. It requires Serein to be closed and refuses to run
when the source tree has uncommitted changes. If an upstream change conflicts with a local patch,
Git stops at the conflict instead of discarding either side; resolve and commit it, then rerun the
script.

The script does not push to GitHub. Local custom commits remain in the customization branch and
are included when later nightly tags are merged. Settings and account data are stored outside the
installation folder and are not touched by this workflow.
