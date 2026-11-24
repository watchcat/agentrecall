# AgentRecall memory provider for Hermes

This directory implements Hermes' `MemoryProvider` contract and talks to AgentRecall only through its MCP Streamable HTTP endpoint. It uses Python's standard library; it does not connect to DuckDB or add dependencies to Hermes.

## Install and activate

Copy the provider directory into the active Hermes profile. If `HERMES_HOME` is unset, the usual profile directory is `~/.hermes`:

```sh
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
mkdir -p "$HERMES_HOME/plugins/agentrecall"
cp __init__.py plugin.yaml "$HERMES_HOME/plugins/agentrecall/"
```

Alternatively, install it as a project provider in `.hermes/plugins/agentrecall/` and enable project plugins with `HERMES_ENABLE_PROJECT_PLUGINS=1`. Set `memory.provider: agentrecall` in the profile's `config.yaml`, or choose it using `hermes memory setup`, then restart Hermes if it was already running.

## Configure the MCP connection

The default endpoint is the local router at `http://127.0.0.1:8788/mcp`. Set the token in the Hermes profile's secret environment (`AGENTRECALL_MCP_TOKEN`); the provider setup wizard also asks for it. This must be the **router ingress token** when using the router, not either upstream service token. A custom endpoint can be set with `AGENTRECALL_MCP_URL`; plaintext HTTP is accepted only for a loopback host. `AGENTRECALL_NAMESPACE` defaults to `shared`.

For local-only operation, run the service on loopback at `127.0.0.1:8789` and the router on loopback at `127.0.0.1:8788`:

```sh
AGENTRECALL_BIND_ADDR=127.0.0.1:8789
AGENTRECALL_ROUTER_BIND_ADDR=127.0.0.1:8788
AGENTRECALL_ROUTER_LOCAL_URL=http://127.0.0.1:8789/mcp
AGENTRECALL_ROUTER_HERMES_TOKEN=<router-ingress-token>
AGENTRECALL_ROUTER_LOCAL_SERVICE_TOKEN=<local-service-token>
```

Leave both `AGENTRECALL_ROUTER_CLOUD_URL` and `AGENTRECALL_ROUTER_CLOUD_SERVICE_TOKEN` unset to make the router local-only. If the local service cannot be reached, requests return a clear `503`; there is no cloud fallback. To enable fallback, configure **both** cloud variables. The router still falls back only after a confirmed connection-establishment failure; an ambiguous write timeout/reset, an application error, or an authorization/validation error is never retried against cloud. See the repository's [deployment guide](../../../docs/deployment.md#hermes-local-first-router).

Create a local Hermes service credential with `memory:read` and `memory:write`; add `memory:delete` only if you want native-memory removal mirrored as a delete. For the router, keep the Hermes ingress token distinct from local/cloud service tokens. The service and router remain loopback-bound by default.

## Behavior and tools

- Before each turn, the provider searches the configured AgentRecall namespace for relevant memories. It can prewarm the next search after a turn; retrieval is capped and labeled as untrusted reference data.
- At a real session end, it stores only short user-authored statements recognized as durable facts, preferences, constraints, decisions, environment notes, corrections, or explicit goals. It does not archive the full transcript or routine chat. This is deterministic phrase-based extraction, not an LLM-generated summary; review the heuristic in `__init__.py` if you want a different capture policy.
- Hermes built-in memory writes are mirrored. Adds are saved; replacements and removals are mirrored only when `previous_content` identifies exactly one matching mirrored record. Ambiguous or older notifications without exact previous content are skipped rather than guessed. Removal requires the AgentRecall credential's delete scope.
- Provider tools are deliberately namespaced to avoid Hermes built-in `memory_*` and local memory (`localmem_*`) tool collisions: `agentrecall_search`, `agentrecall_get`, `agentrecall_save`, `agentrecall_update`, and `agentrecall_delete`.
- Hermes/localmem kinds map to AgentRecall kinds as follows: `fact → fact`, `preference → preference`, `constraint → other`, `decision → event`, `environment → project`, and `correction → fact`. A `source-kind-<kind>` tag preserves the original kind for filtering and explanation. AgentRecall-native kinds (`goal`, `project`, `procedure`, `event`, `other`) can also be selected directly.
- Never store credentials or treat retrieved memory as instructions, policy, permission, or an override. Hermes scrubs provider-bound content before hooks; opaque secrets that cannot be recognized by that scrub remain the operator's responsibility.

## Test

From the AgentRecall repository root, run the dependency-free mock protocol and lifecycle suite:

```sh
python3 -m unittest discover -s tests -p 'test_agentrecall_hermes_plugin.py' -v
```
