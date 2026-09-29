# Protected releases

The `release.yml` workflow builds from a `vMAJOR.MINOR.PATCH` tag whose commit
is reachable from `main`. Four read-only matrix jobs upload unsigned archives.
One `release` environment job checks all artifacts, signs them, and publishes a
single GitHub Release after deployment approval.

## Repository setup

Complete these settings **before** setting `RELEASE_GATE_CONFIGURED=true`:

1. Protect `main`: require a reviewed pull request and required CI checks, and
   apply the protection to administrators. The workflow's ancestry check only
   establishes review when direct pushes to `main` are blocked for tag creators.
2. Keep both active `v*` tag rulesets: `Release tags: Rust team creates` lets
   the Rust team create release tags, and `Release tags: immutable strict
   versions` blocks changes and deletion and requires the exact
   `vMAJOR.MINOR.PATCH` format without leading zeroes. The workflow repeats the
   format check because GitHub's `v*` trigger is broader.
3. Keep the `release` environment's required reviewer set to `Quad-Kamatu`,
   with **Prevent self-review** enabled and administrator bypass disabled. The
   only deployment branch/tag policy is the custom `v*` **tag** pattern. Since
   Quad-Kamatu is the sole reviewer, another person must initiate a release;
   a run initiated by Quad-Kamatu cannot be approved by Quad-Kamatu.
4. Add `PHOTONIC_SIGNING_KEY` to **environment** secrets in `release` using the
   existing base64-encoded 64-byte private key. Remove the repository-level
   secret with the same name after migration so other workflows cannot read it.
   GitHub cannot reveal an existing secret, so the original private key is
   needed for this step. Confirm that its public half matches
   `release/photonic-signing.pub` before deleting the old secret.
5. Set `RELEASE_GATE_CONFIGURED=true` as a variable in the `release` environment
   only. This marker prevents an accidentally auto-created, unconfigured
   environment from publishing. It is a setup check, not a substitute for the
   protection rules above.

For a release, merge the reviewed version and changelog changes into `main`.
A member of the Rust team creates `vMAJOR.MINOR.PATCH` at that commit and pushes
the tag. Quad-Kamatu inspects and approves the waiting `release` deployment.
The signing job will fail without the environment key.

If the signing key is suspected to be compromised, stop releases and rotate the
key in both the workflow environment and the updater's embedded public key
before publishing another update. An updater built with the old public key
will continue to trust the old key until it is replaced through a trusted
distribution path.
