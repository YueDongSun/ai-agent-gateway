# Privacy-safe public snapshot

The public source boundary is a single audited tree, not this repository's
private Git history. Historical commits, tags, releases, local configuration,
logs, databases, screenshots, and evaluation receipts are not publication
inputs.

## Required gates

From a clean, reviewed private checkout:

```text
npm ci --ignore-scripts
npm ci --prefix ui --ignore-scripts
npm run test:privacy
npm run privacy:audit
npm run release:verify
```

`privacy:audit_history` is expected to block the private legacy history. It is
an archive audit, not a gate that can be waived for a public repository.

After committing the reviewed fixes privately, export the exact commit into a
new, non-existing directory outside this repository:

```text
npm run privacy:export_snapshot -- \
  --source-commit <full-private-commit-sha> \
  --output <absolute-new-directory>
```

The exporter reruns the commit-bound privacy audit, rejects symlinks and
submodules, excludes private historical receipts and release binaries, checks
all local Markdown links, verifies the npm package allowlist, and writes a
`public_snapshot_manifest.json`. The selected commit is accepted only when its
auditor, exporter, Git helper, and export policy exactly match the running
tools. The public manifest contains the selected policy hash, export-tree
digest, per-file hashes/modes/sizes, and redacted gate summaries. It contains
no private commit, Git tree, or commit timestamp. The command response reports
private source provenance only to the caller; do not redirect that response
into the snapshot. The exporter never copies `.git`, ignored files, or
worktree-only state.

Before initializing Git, audit the exported directory directly:

~~~text
cd <absolute-new-directory>
npm run privacy:audit
~~~

This no-Git mode verifies the manifest against every exported file, rejects
unmanifested files, symlinks, tampering, and Git metadata, and repeats content
and npm package-surface checks.

## New repository procedure

1. Review the manifest and complete the no-Git audit shown above.
2. Initialize a brand-new Git repository in the export directory.
3. Configure an intentionally public author identity, preferably a GitHub
   noreply address, before the first commit.
4. Enable GitHub secret scanning, push protection, private vulnerability
   reporting, branch protection, and the tracked `privacy_gate` PR/push check
   before accepting contributions.
5. Do not copy refs, tags, releases, issues, Actions logs, stashes, reflogs,
   bundles, or `.git` objects from the private archive.

Do not use `git push --mirror` from the private repository. A history rewrite
is not equivalent to a history-free snapshot and can leave old tag, release,
cache, fork, or reflog reachability.

## Product privacy boundary

- Credential-bearing local files use private filesystem permissions and remain
  plaintext at rest. Filesystem isolation is not encryption or a keychain.
- Cloud database upload/download is disabled until an authenticated
  client-side encrypted payload format is implemented. Remote connection
  checks require HTTPS, except explicit loopback development endpoints.
- Product logs and externally visible errors use stable codes and bounded
  content-free metadata. Users must not attach raw local logs or databases to
  public issues.
- The public application identifier is `org.aagateway.desktop`. It is a new
  package identity and is not an in-place upgrade of private preview builds
  that used a personalized identifier.
- Marketplace descriptions are PII-scrubbed at both Node collection and native
  refresh boundaries. Repository/author attribution and source provenance stay
  visible; that metadata is not a plugin security audit.

## Release assets

The snapshot intentionally contains no installer, executable, PDB, archive, or
prior GitHub Release asset. Build new assets from the audited snapshot with the
tracked lockfiles. Before publishing an asset, inspect strings and metadata,
verify signatures and hashes, generate an SBOM, and repeat the privacy audit on
the unpacked payload.
