# Security Policy

## Supported versions

The repository is currently a preview. Security and privacy fixes are applied
to the latest commit on the default branch; no older preview is promised a
backport unless its release notes say otherwise.

## Reporting a vulnerability

Do not open a public issue containing credentials, private configuration,
prompt or response content, local paths, logs, databases, or network topology.

Use GitHub's private vulnerability reporting feature for this repository. If
that feature is unavailable, contact the repository owner through a private
channel shown on the GitHub profile without including secret material in the
first message.

Include a minimal reproduction, affected version, stable error codes, and the
least sensitive evidence needed to explain impact. Replace credentials and
private endpoints with synthetic values. The maintainers may ask for a
separately encrypted artifact when logs or databases are essential.

## Scope

Reports about credential storage, redaction, local proxy authorization, MCP or
DSH isolation, configuration backups, update/signing boundaries, and unsafe
cross-window command access are in scope. Provider availability, unsupported
third-party plugins, and expected rate limits are not vulnerabilities unless
they cross an AAG security boundary.
