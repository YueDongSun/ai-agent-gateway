# Upstream Router Configuration

This module preserves reusable LiteLLM routing configuration behavior without
depending on CC Switch, OpenClaw, their databases, or provider credentials.
The example contains only fictional `.example.test` endpoints and environment
variable names.
Replace every `replace_me` model identifier, fictional endpoint, and
placeholder capacity with the adapter, model ID, endpoint, and confirmed
capacity supported by the intended LiteLLM installation before use.

Create a private JSON config from [config_example.json](config_example.json),
then set its referenced provider key variables and `ROUTER_MASTER_KEY` in the
environment of the process that will own LiteLLM. Config files contain env
variable names only; never put key values in the JSON file.

Set `$python_executable` and `$router_config_path` to verified absolute paths
before running these commands. Set `$litellm_config_path` to an absolute path
inside a directory private to the current user.

Choose exactly one CLI action: `--check` validates and reports readiness, and
`--output` creates the LiteLLM JSON file. Combining the actions or omitting
both is an argument error; checking never writes output. JSON object fields
must be unique at every level, so duplicate names are rejected even if a later
duplicate would appear to correct the earlier value.

Validate the input and report environment/LiteLLM readiness without exposing
values:

```powershell
& $python_executable -B -m scripts.upstream_router --config $router_config_path --check
```

To create a LiteLLM-compatible JSON file, supply an absolute output path that
does not already exist and is inside a directory private to the current user:

```powershell
& $python_executable -B -m scripts.upstream_router --config $router_config_path --output $litellm_config_path
```

On Unix, the created file uses mode 0600. On Windows, it inherits its parent
directory ACL, so the caller must choose a private directory. Creation uses
exclusive mode and will not overwrite an existing file. The rendered config
still contains only `os.environ/NAME` references.

This tool never starts LiteLLM. A caller may use the printed command template
with its explicit Python interpreter and generated config path, binding only
the configured loopback host and port, or build a launch specification through
`router_runtime.prepare_launch_spec`. The command uses
`litellm.proxy.proxy_cli` and includes explicit host/port arguments so the
proxy does not choose a wider default bind address. The
caller owns foreground process execution, cancellation, cleanup, health
checks, and any restart policy. There is no autostart, scheduler, detached
child, supervisor, or automatic port conflict handling in this module.
`router_runtime` replaces inherited `NO_PROXY` and `no_proxy` with loopback-only
values to avoid malformed inherited proxy bypass entries. The prepared
environment also sets `LITELLM_LOG=ERROR` and `NO_COLOR=1` in its copy. The
caller still owns third-party runtime log handling. The prepared environment
may contain credentials; do not print or serialize the full launch
specification.

Schema version 1 requires a loopback listener, unique aliases, valid
environment variable identifiers, explicit upstream model/base/key variable
for each model, and positive `max_input_tokens`. Fallback targets must name a
different configured alias exactly once. Upstream base URLs must use HTTP(S)
and cannot contain userinfo, query, or fragment components.

Run the module tests with the project's verified Python runtime:

```powershell
& $python_executable -B -m unittest scripts.upstream_router.test_router_config scripts.upstream_router.test_router_runtime scripts.upstream_router.test_router_cli
```
