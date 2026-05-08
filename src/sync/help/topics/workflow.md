bisque-sync workflow
====================

bisque-sync turns a directory of YAML files into SaaS state. You edit files,
run `plan` to see what will change, run `apply` to push it.

Lifecycle:

  1. `bisque-sync init`
       Scaffolds `bisque.yaml`, `.bisque/` (state), a `.gitignore` entry,
       and appends an orientation stanza to the workspace `CLAUDE.md`.

  2. `bisque-sync import klaviyo templates`
       One-time bootstrap. Reads every template already in the Klaviyo
       account, writes `integrations/klaviyo/templates/<slug>.yaml`, and
       records the remote IDs in `.bisque/state.db`.

  3. Edit YAML (or the source referenced by `html.command`).

  4. `bisque-sync plan`
       Diffs YAML + rendered HTML against the state DB. Prints CREATE,
       UPDATE, or NOOP for each resource.

  5. `bisque-sync apply`
       Executes the plan. Calls `klaviyo_update_template` /
       `klaviyo_create_template` via the bisque backend. Updates the
       state DB after each successful call.

Examples:

    # Orient in an unfamiliar repo
    bisque-sync explain --json

    # Preview pending changes without network
    bisque-sync plan

    # Apply without confirmation (CI-friendly)
    bisque-sync apply --auto-approve

Structured output:

    All commands accept `--json`. The envelope is `{ok, data, error}`.
    Errors carry `code` (e.g. E_RENDER_FAILED) and `remediation`.
