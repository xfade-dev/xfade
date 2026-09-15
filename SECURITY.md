# Security policy

## Reporting a vulnerability

Please do **not** open a public issue for security vulnerabilities.

Instead, report them privately via GitHub's Security Advisory feature:

1. Go to the **Security** tab of this repository.
2. Click **Report a vulnerability**.
3. Describe the issue in as much detail as possible (affected versions, steps to reproduce, potential impact).

We aim to acknowledge reports within 48 hours and provide a fix or a timeline as soon as possible.

## Scope

Security-relevant areas of this project include, but are not limited to:

- **Secret handling**: API keys are stored in the system keyring (`store/secrets.rs`). Any path that could leak, log, or forward a key unintentionally is in scope.
- **The local proxy** (`proxy/`): the `auth_token` guard, header stripping (`STRIP_HEADERS`), and the request-forwarding path must not leak credentials to upstream or to unintended clients.
- **Config writers** (`adapters/`): writing tool config files must not accidentally expose secrets in plaintext beyond what the target tool itself expects.

## Coordinated disclosure

We will keep the reporter informed of progress and credit them in the release notes unless they request otherwise.
