# Repository privacy and public snapshot tools

These tools prepare a reviewable, history-free public source snapshot. They do
not change Git refs, create commits, push, publish packages, or rewrite the
private repository history.

## Current-tree audit

`npm run privacy:audit` scans every tracked file plus every non-ignored
worktree file. It checks credential formats, private-key headers, JWTs,
credential-bearing URIs, common literal secret assignments, UTF-8 and UTF-16
text, arbitrary drive/UNC/WSL paths, email addresses, private IPv4/IPv6 and
Tailnet addresses, sensitive filenames, ignore rules, generated-catalog size,
package privacy metadata, and the actual `npm pack --dry-run` manifest.

The JSON report never includes a raw match. Each finding contains only severity,
category, repository-relative path, line number (zero for binary content), and
a deterministic location fingerprint. A dirty scan reports the HEAD and index
identities separately from `scanned_tree_sha256`; the HEAD tree is never
presented as the scanned worktree.

Synthetic security canaries are exempt only in explicit test contexts with
fixture markers or reserved test domains. Production source, documentation,
catalog data, and ordinary test content remain subject to the same blockers.

## Private-history audit

`npm run privacy:audit_history` enumerates every path in every unique reachable
tree, scans each unique blob under all of its historical paths, checks every
historical sensitive filename, and scans commit author/committer email
metadata. The existing private repository intentionally retains historical
identity and old private paths, so this command is expected to fail. It is
evidence that those refs must not be copied to the public repository.

## History-free export

Run the exporter only after the private-history repair commit has passed its
gates:

```text
npm run privacy:export_snapshot -- --output <absolute_outside_repo_path>
```

The default form refuses a dirty source tree. To export an already tested commit
while unrelated work is present, supply its full 40-character hash:

```text
npm run privacy:export_snapshot -- --output <absolute_outside_repo_path> --source-commit <full_commit_hash>
```

The exporter audits the selected source first, rejects symbolic links and path
traversal, refuses an existing destination or a destination inside the source
tree, exports tracked blobs only, removes release assets and internal receipts,
checks local Markdown links and the npm pack surface, and writes
`public_snapshot_manifest.json`. An explicit source commit is accepted only
when its auditor, exporter, Git helper, and export policy exactly match the
running tools. The public manifest binds the selected policy, export-tree
digest, every file hash/mode/size, the redacted privacy result, and the
link/package results. It deliberately contains no private commit, tree, or
timestamp. The command response reports private source provenance only to the
caller; do not redirect that response into the snapshot. No `.git` directory
or historical ref is copied.

The exported directory is auditable before Git initialization:

~~~text
cd <absolute_outside_repo_path>
npm run privacy:audit
~~~

Directory mode verifies every manifest-bound file, rejects unmanifested files,
symlinks, tampering, and `.git` metadata, and repeats the content and package
surface scans without depending on a parent repository.

The exact exclusion and size policy is versioned in
[public export policy](public_export_policy.json).

## Safe cc-switch database diagnostic

`inspect_cc_switch_db.py --database <path>` opens an explicitly supplied
SQLite file in read-only/query-only mode. It reports only allowlisted table
presence and row counts. It never prints the path, provider identities,
endpoints, settings values, or credentials.
