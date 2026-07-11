# Code signing & provenance

Status: **groundwork only — signing is a post-v0.1 purchase decision.** What
ships today at zero cost: GitHub **build-provenance attestations** over every
release artifact (wired into `release.yml`, runs only at the future `v0.1.0`
tag). Everything below documents the exact paid path so turning it on later is
configuration, not research.

## What users see without signing

- **Windows:** SmartScreen shows "Windows protected your PC" for a downloaded
  unsigned binary until enough reputation accrues; `winget`/`cargo install`
  users are unaffected.
- **macOS:** Gatekeeper blocks unsigned, un-notarized binaries downloaded via a
  browser (`xattr -d com.apple.quarantine` is the manual escape hatch);
  Homebrew/cargo installs are unaffected.
- **Everywhere:** the GitHub attestation already lets anyone verify an artifact
  was built by this repo's workflow from a specific commit:
  `gh attestation verify <file> --repo cc1a2b/tazamun`.

## Windows — Authenticode

**Certificate options (annual cost, ballpark):**

| Type | Cost/yr | SmartScreen | Notes |
| --- | --- | --- | --- |
| OV code signing | ~$80–250 (Certum/Sectigo/SSL.com) | reputation builds over time | Since June 2023 the key MUST live on a HSM/USB token or cloud HSM — no plain PFX files. |
| EV code signing | ~$250–500 | immediate reputation | Hardware token or cloud signing (Azure Trusted Signing ≈ $9.99/mo is the budget EV-equivalent). |

**Manual signing (what any option boils down to):**

```powershell
# Sign with SHA-256 and a RFC 3161 timestamp (the timestamp keeps the
# signature valid after the certificate expires — never skip it).
signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 `
  /n "Your Cert Subject Name" target\dist\tazamun.exe
signtool verify /pa target\dist\tazamun.exe
```

**Where it hooks into cargo-dist:** cargo-dist has first-class SSL.com eSigner
support — set in `Cargo.toml`:

```toml
[workspace.metadata.dist]
# ssldotcom-windows-sign = "prod"   # placeholder — enable when a cert exists
```

and provide `SSLDOTCOM_USERNAME` / `SSLDOTCOM_PASSWORD` /
`SSLDOTCOM_CREDENTIAL_ID` / `SSLDOTCOM_TOTP_SECRET` as repo secrets; dist then
signs the Windows artifacts inside the release workflow. For other providers
(Azure Trusted Signing, a local token) add a `signtool` step to `release.yml`
between build and upload.

## macOS — Developer ID + notarization

**Cost:** Apple Developer Program, **$99/yr** (individual is fine).

**One-time setup:** create a *Developer ID Application* certificate in the
Apple Developer portal, install it in a keychain (CI: export as `.p12`, import
in the workflow), and mint an App Store Connect API key for `notarytool`.

**Manual flow:**

```bash
# 1. Sign with hardened runtime (required for notarization).
codesign --force --options runtime --timestamp \
  --sign "Developer ID Application: NAME (TEAMID)" tazamun

# 2. Notarize (zip first; notarytool wants an archive).
ditto -c -k tazamun tazamun.zip
xcrun notarytool submit tazamun.zip \
  --key AuthKey_XXXX.p8 --key-id XXXX --issuer YYYY --wait

# 3. Staple the ticket so offline Gatekeeper checks pass.
#    (Stapling attaches to bundles/dmg/pkg; a bare binary relies on the
#    online check — ship a .dmg or .pkg if stapling matters.)
xcrun stapler staple tazamun.dmg
```

**Where it hooks into cargo-dist:** cargo-dist can codesign macOS artifacts
when the certificate secrets exist — placeholders:

```toml
[workspace.metadata.dist]
# macos-sign = true   # placeholder — enable when the Developer ID cert exists
```

with `CODESIGN_CERTIFICATE` / `CODESIGN_CERTIFICATE_PASSWORD` repo secrets;
notarization currently needs an explicit `notarytool` step in `release.yml`
after the sign step (keep the API key in secrets).

## Provenance attestations (live now, free)

`release.yml` runs `actions/attest-build-provenance` over the built artifacts
and checksums at tag time (permissions `id-token: write` +
`attestations: write`). Verification for any downloaded artifact:

```bash
gh attestation verify tazamun-x86_64-pc-windows-msvc.zip --repo cc1a2b/tazamun
```

## Decision checklist (when v0.1.0 nears)

- [ ] Windows: Azure Trusted Signing (cheapest EV-grade) vs OV token — decide
      by whether SmartScreen reputation matters on day one.
- [ ] macOS: join the Developer Program only if browser-download distribution
      matters (Homebrew users never hit Gatekeeper).
- [ ] Enable the placeholders above + add secrets; `dist plan` must stay clean
      either way.
