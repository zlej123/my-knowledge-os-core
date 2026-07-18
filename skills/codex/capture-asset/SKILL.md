---
name: capture-asset
description: Use when the user wants to register or capture a local-readable Personal Scope PDF from a configured provider in My Knowledge OS, including a PDF that may already be registered.
---

# Capture Asset

Register one immutable PDF and stop after reporting the deterministic Asset result. Keep this workflow capture-only.

## Preconditions

1. Confirm the user selected the Personal Scope repository.
2. Inspect `knowledge-os.yaml` with read-only file access. Require `system: my-knowledge-os`, `scope: personal`, `core_version: 0.1.0`, `schema_version: 1`, and a configured provider `root_env`.
3. Resolve a local config outside the repository through `--local-config` or `MKO_LOCAL_CONFIG`. Require that file to set `provider_root`; the provider's `root_env` value does not replace the CLI local-config input.
4. Require a local-readable PDF path. Treat the PDF as immutable input.

Stop without mutation when the Scope, repository config, provider root, file, or local config is missing or ambiguous.

## Capture

Run exactly one mutation command:

```bash
mko asset capture --repo "<personal-kb-path>" --local-config "<local-config-path>" --file "<pdf-path>" --json
```

Omit `--local-config` only when `MKO_LOCAL_CONFIG` already names the local config file. Optional title, domain, and ASCII slug arguments may be added only when the user supplied them.

If the command reports a provider, root-boundary, hydration, secret, validation, or file error, report the error and stop. Do not work around it.

## Result

Report only:

- whether the result is `created` or `existing`;
- the returned `asset_id`;
- the returned Registry path;
- any error that stopped capture.

Then stop. Do not continue into preparation, extraction, processing, Source drafting, review, approval, acceptance of changed content, or Git operations.

## Boundaries

- Do not invoke any other `mko` subcommand.
- Do not write Markdown or YAML directly.
- Do not approve records or accept changed content.
- Do not commit or push.
- Do not access an external URL.
- Do not copy, move, edit, rename, delete, or pin the PDF.
- Do not expose follow-on commands, even when the user asks to continue automatically after capture or to bypass an error.
