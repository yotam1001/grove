# winget manifest template — waiting on signed releases

**Do not submit these files yet.** They are a template for
[winget-pkgs](https://github.com/microsoft/winget-pkgs), kept here so the
package is ready once Grove has:

1. **Signed releases.** Grove's release executables are not Authenticode-signed
   yet, and winget's validation (and SmartScreen) treat unsigned installers
   poorly. The in-app updater only checks SHA-256 for the same reason.
2. **A stable (non-beta) release.** Releases are currently pre-releases.
3. **Owner approval** to publish under the `yotam1001.Grove` identifier.

The template installs the portable release ZIP (`grove-music.exe` inside it),
which needs no silent-install switches. `GroveSetup.exe` is interactive, so it
would first need a silent mode before it could be the winget installer.

## Filling it in for a release

The release workflow (`.github/workflows/release.yml`) prints the values.
Replace every `{{…}}` placeholder:

| Placeholder | Value |
| --- | --- |
| `{{VERSION}}` | Release version without the `v`, e.g. `0.2.0` |
| `{{ZIP_SHA256}}` | Upper-case SHA-256 of `Grove-<version>-windows-x64.zip` (from `SHA256SUMS.txt` on the release) |
| `{{RELEASE_DATE}}` | `YYYY-MM-DD` |

Then validate locally with `winget validate --manifest packaging/winget` and
test with `winget install --manifest packaging/winget` in a Windows Sandbox
before opening a winget-pkgs pull request.
