# Publishing

The `ci.yml` workflow runs the Linux and macOS checks for every `v*` tag. Its
crates.io job publishes only after `Required` passes, the tag matches the crate
version, and the tagged commit belongs to the default branch. An existing
registry version is left unchanged.

Publication uses GitHub OIDC through `rust-lang/crates-io-auth-action`. The
registry credential is short-lived and revoked when the job ends. There is no
stored publication token, approval environment, or recurring sign-in.

The job stays disabled until these one-time registry prerequisites are complete:

1. Publish the first `apple-foundation` version from a verified release tag
   using the registry's initial-publication authentication.
2. In the crate's settings, add the GitHub trusted publisher: owner `hraness`,
   repository `apple-foundation`, workflow `ci.yml`, with no environment.
3. Enable the job with
   `gh variable set CRATES_IO_PUBLISH --repo hraness/apple-foundation --body true`.

Keep account two-factor authentication enabled. Do not enable the repository
variable before the crate and trusted publisher exist. Later releases use a
strictly increasing crate version and an immutable `v<version>` tag; pushing
that tag runs the checks and publishes without a person approving the release.
