# Security policy

## Reporting a vulnerability

Please do **not** open a public issue for security vulnerabilities.

Instead, report them privately via GitHub's Security Advisory feature:

1. Go to the **Security** tab of this repository.
2. Click **Report a vulnerability**.
3. Describe the issue in as much detail as possible (affected versions, steps to reproduce, potential impact).

Alternatively, you can email our security response team directly at:
**`security@xfade.dev`** (or PGP-encrypted to our published key).

## Response SLA (Service Level Agreement)

As defined in our Trust Baseline (T8):

- **48 Hours**: Initial triage and acknowledgment of valid security reports.
- **7 Days**: Fix or mitigation for critical/high severity vulnerabilities.
- **90 Days**: Coordinated public disclosure and release notes publication.

## Scope & Threat Model

Security-relevant areas of this project include:

- **Credential Isolation & Secret Handling**: Upstream API keys are stored in the system keyring (`store/secrets.rs`) and proxy memory only. Client configuration files receive only local virtual tokens (`xf-tok-*`). Raw API keys must never be written to client config files, logs, or error events.
- **Local Gateway Defense**: Inbound credential stripping (`STRIP_HEADERS`), loopback Host/Origin validation, and mandatory constant-time token authentication.
- **Outbound Egress Guard**: Egress domain filtering to guard against SSRF, unintended redirect following, or data exfiltration.
- **Filesystem Permissions (T7)**: Restricted owner-only file permissions (`0600` on Unix; protected owner/SYSTEM DACLs on Windows) for token databases, state, and secrets.
- **Supply Chain Integrity**: Sigstore keyless signatures, GitHub build provenance attestations, CycloneDX SBOM generation, and `cargo-deny` security gate in CI.

### Out of Scope
- **Same-UID host processes**: A malicious local process running under the same OS user identity has access to read the user's files and keychain. xfade cannot prevent same-UID local compromise.
- **Compromised root/kernel or hijacked client binaries**: Documented as external client-side trust boundaries.

## Release Integrity

CLI release artifacts are covered by a SHA-256 manifest signed keylessly by
this repository's GitHub Actions release workflow using Sigstore. GitHub also
records build-provenance attestations for the artifacts. Verification
instructions and the precise signer identity are documented in
[`RELEASE.md`](RELEASE.md#signing).
