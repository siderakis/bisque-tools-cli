---
name: bisque.tools
description: Call Bisque Agent API endpoints directly for tool discovery and tool execution without MCP.
---

# Bisque Tools

You have access to the user's Bisque integrations (Google Calendar, Gmail, Tesla,
weather, documents, and more) via the `bisque` CLI.

## Setup

Credentials are loaded from `~/.bisque/config.json`:

```json
{
  "activeProfile": "default",
  "profiles": {
    "default": {
      "userId": "your-user-id",
      "apiKey": "bisque_live_...",
      "baseUrl": "https://bisque.tools"
    }
  }
}
```

Override the profile with `--profile <name>` or `BISQUE_PROFILE` env var.
You can also set `BISQUE_USER_ID` and `BISQUE_API_KEY` as environment
variables (they take precedence over the config file).

## Workflow

### 1. Sync per-integration skills (recommended)

Run this once at session start to generate a separate skill directory per
connected integration:

```bash
bisque sync
```

This calls the Bisque API, discovers which integrations the user has
connected, and generates a `bisque-<integration>/` skill directory for each
one (e.g., `bisque-google-analytics/`, `bisque-reddit-ads/`). Each generated
skill has its own `SKILL.md` and `tools.json` scoped to that integration.

If an integration is disconnected since last sync, its skill directory is
removed automatically.

### 2. Discover available tools

If you prefer a flat tool list instead of per-integration skills:

```bash
bisque tools
```

This prints each available tool on one line (`name — description`). For full
tool schemas, add `--json`:

```bash
bisque tools --json
```

If you need the full bootstrap payload (user documents, skill descriptions,
integration status), use:

```bash
bisque bootstrap
```

### 3. Execute a tool

```bash
bisque call <toolName> --args '<json>'
```

You can also pipe args via stdin:

```bash
echo '{"location":"New York, NY"}' | bisque call weather_current
```

### 4. Parse the result

Tool call responses are JSON on stdout with this shape:

```json
{
  "status": "succeeded",
  "summary": "Human-readable result summary.",
  "data": {}
}
```

- `status` is `"succeeded"`, `"failed"`, or `"denied"`.
- `summary` is always present — use it when relaying results to the user.
- `data` is optional and contains structured output (events, messages, etc.).

Errors also come as JSON on stdout:

```json
{
  "error": { "code": "TOOL_NOT_AVAILABLE", "message": "..." }
}
```

Script-level errors (missing credentials, bad args) go to stderr.

### 5. Check setup health

```bash
bisque doctor
```

This validates credentials, tests API auth, lists connected vs. available
integrations, and flags stale generated skill directories.

### 6. Connect new integrations

If the task at hand would benefit from an integration the user hasn't
connected yet, check the **Available Integrations** skill (generated
automatically by `bisque sync`) for a list of unconnected integrations.

To connect a new integration, run:

```bash
bisque connect <integration-name>
```

This opens the browser to the OAuth/setup page. After the user completes
setup, run `bisque sync` to pull in the new tools.

## Guidelines

- Always run `tools` or `sync` first before calling anything. Do not guess tool names.
- Use the `summary` field to respond to the user — do not dump raw `data`
  unless they ask for details.
- If a tool returns `"denied"`, tell the user the integration may need
  re-authorization in the Bisque web app.
- If a Google tool returns a **403 "insufficient authentication scopes"**
  error, the user needs to grant additional OAuth scopes. Open the scope
  expansion URL to auto-trigger the consent screen:
  ```bash
  open "https://bisque.tools/integrations?integration=<integrationId>&expand_scopes=<scope1>,<scope2>"
  ```
  After the user approves, retry the tool call.
- If auth fails (`INVALID_API_KEY` or `MISSING_USER_ID`), tell the user to
  check their credentials and rotate the key at bisque.tools if needed.
- If a tool returns `TOOL_NOT_AVAILABLE`, re-run `sync` to refresh
  integration state, then retry.
- Use `--pretty` if you need human-readable JSON output.
- If a task could benefit from an unconnected integration, proactively
  suggest it to the user and offer to run `bisque connect <name>`.
