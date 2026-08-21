# Courier Desktop releases

Courier Desktop release artifacts are built on native GitHub-hosted runners when a GitHub Release is published, then attached to that existing release. Mac artifacts are signed and notarized during the build.

The workflow produces:

- a signed and notarized DMG for Apple Silicon Macs;
- a signed and notarized DMG for Intel Macs;
- an unsigned Windows x64 NSIS installer;
- a Linux x64 AppImage.

Windows signing is intentionally not configured yet. Do not present an unsigned Windows build as production-ready; Windows may display a SmartScreen warning.

## Apple account prerequisites

Direct distribution outside the Mac App Store requires a paid Apple Developer Program membership. The Apple Developer Account Holder must create the `Developer ID Application` certificate.

1. On a trusted Mac, create a certificate signing request in Keychain Access.
2. In Apple Developer Certificates, Identifiers & Profiles, create a **Developer ID Application** certificate from that request.
3. Install the downloaded certificate on the Mac. In Keychain Access, confirm that it appears under **My Certificates** with its private key nested beneath it.
4. Export the certificate and private key as a password-protected `.p12` file.
5. In App Store Connect, open **Users and Access → Integrations** and create an API key with Developer access. Record its issuer ID and key ID, then download the `.p8` private key. Apple permits the private-key download only once.

The application identifier is `co.icyseas.courier`. The current workflow distributes the app directly and does not submit it to the Mac App Store.

### Local Mac signing

Local certificate material may be staged under the repository's ignored
`.signing/apple/` directory and imported into the login keychain. Courier's local
build helper selects a `Developer ID Application` identity when available and
otherwise accepts an `Apple Development` identity with an explicit warning:

```bash
cd apps/courier-desktop
npm run tauri:build:mac-local
```

The helper intentionally fails before compiling when `security find-identity -v
-p codesigning` reports no usable identity. A downloaded `.cer` contains only the
public certificate; it becomes a signing identity only when Keychain also has its
matching private key. Export that certificate and private key from the Mac that
created the CSR as a password-protected `.p12`, or revoke/reissue it from a CSR
created on the build Mac.

An `Apple Development` signature is for local testing only. It does not replace
the `Developer ID Application` identity and App Store Connect API key required by
the release workflow for external distribution and notarization.

## Configure GitHub secrets

Create a GitHub Actions environment named `desktop-release`, add a required reviewer, and store these as environment secrets:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64-encoded Developer ID `.p12` file |
| `APPLE_CERTIFICATE_PASSWORD` | Password assigned while exporting the `.p12` file |
| `APPLE_API_ISSUER` | App Store Connect API issuer ID |
| `APPLE_API_KEY` | App Store Connect API key ID |
| `APPLE_API_PRIVATE_KEY` | Base64-encoded App Store Connect `.p8` private key |

Encode the files without line wrapping:

```bash
openssl base64 -A -in DeveloperIDApplication.p12 -out DeveloperIDApplication.p12.base64
openssl base64 -A -in AuthKey_KEYID.p8 -out AuthKey_KEYID.p8.base64
```

Copy each encoded file's contents into its corresponding GitHub secret. Never commit the `.p12`, `.p8`, their base64 encodings, or their passwords. Retain the source credentials in the organization's approved credential store; GitHub Secrets are a CI delivery mechanism, not the authoritative backup.

Restrict creation of tags matching `courier-v*` with a repository ruleset. The protected `desktop-release` environment ensures a release reviewer must approve the jobs before the signing credentials become available.

The workflow creates a temporary keychain on each macOS runner, imports the Developer ID identity, signs the application, submits it to Apple for notarization, staples the notarization ticket, and deletes the temporary keychain after the build.

## Create a release

Keep these versions identical before tagging:

- `Cargo.toml` under `[workspace.package]`;
- `apps/courier-desktop/package.json`;
- `apps/courier-desktop/src-tauri/tauri.conf.json`.

Commit the version change and create the matching tag:

```bash
git tag courier-v0.1.0
git push origin courier-v0.1.0
```

The workflow rejects a release tag that does not exactly match `courier-v<application-version>`. Create a GitHub Release for that existing tag and publish it (mark it as a prerelease while the beta is ongoing). Publication starts the workflow; the four artifacts are uploaded to that release after they build, and the macOS artifacts are notarized automatically. If a job fails, use **Re-run failed jobs** from the workflow run after correcting the issue.

## Release verification

After the publication-triggered workflow completes:

1. Confirm all four workflow jobs succeeded from the tagged commit.
2. On both Mac architectures, inspect the signature and notarization ticket:

   ```bash
   codesign --verify --deep --strict --verbose=2 "/Applications/Icy Seas Courier.app"
   spctl --assess --type execute --verbose=2 "/Applications/Icy Seas Courier.app"
   xcrun stapler validate "/Applications/Icy Seas Courier.app"
   ```

3. Install the Windows package on a clean Windows x64 account and the AppImage on the oldest supported Linux distribution.
4. Run the complete acceptance sequence in `docs/beta-deployment.md` against the beta Registry.
5. Record the Courier version, artifact architecture, operating system, transfer ID, and result.

If either macOS job lacks a certificate or notarization secret, it fails before compiling instead of publishing an unsigned DMG.
