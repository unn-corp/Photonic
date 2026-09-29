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
2. Add an active tag ruleset for `v*`. Restrict creation and updates to the
   designated release managers; block deletion and force updates. The workflow
   also rejects tags that are not exactly `vMAJOR.MINOR.PATCH` without leading
   zeroes. GitHub's `v*` trigger and ruleset patterns are broader than that
   format, so the runtime check is required.
3. Create a `release` environment. Require at least two trusted reviewers and
   enable **Prevent self-review**. Disable administrator bypass of deployment
   protection. Set deployment branch/tag restrictions to a custom `v*` **tag**
   pattern only. Verify the environment shows these rules before proceeding.
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

For a release, merge the reviewed version and changelog changes into `main`,
create `vMAJOR.MINOR.PATCH` at that commit, and push the tag. An authorized
reviewer who did not initiate the run must inspect and approve the waiting
`release` deployment. The signing job will fail without the environment key.

If the signing key is suspected to be compromised, stop releases and rotate the
key in both the workflow environment and the updater's embedded public key
before publishing another update. An updater built with the old public key
will continue to trust the old key until it is replaced through a trusted
distribution path.
