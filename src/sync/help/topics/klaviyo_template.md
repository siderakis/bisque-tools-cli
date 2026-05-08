bisque-sync: klaviyo template kind
==================================

Template YAMLs live at `integrations/klaviyo/templates/<slug>.yaml`. The
filename stem becomes the resource name (with `-` normalized to `_`).

Shape (see `bisque-sync schema klaviyo template` for the JSON Schema):

    kind: template
    name: "Customer At Risk — Reminder (AR1)"
    html:
      render: exec
      command:
        - bun
        - functions/emails/scripts/render-one.tsx
        - --export
        - ReminderEmail
        - functions/emails/src/emails/segments/customer-at-risk/1-reminder.tsx

Rules:

  - `name`: sent to Klaviyo as `attributes.name`. Rename here to rename there.
  - `html.render`: must be `exec` in the prototype.
  - `html.command`: argv executed in the workspace root. stdout is
    captured as HTML bytes. Anything printed to stderr is ignored unless
    the process exits non-zero.
  - Missing `command: []` produces E_RENDER_FAILED with remediation.

Hashing:

  A template's desired state is the sha256 of (canonical YAML || rendered
  HTML). `plan` reports UPDATE whenever either input changes.

Examples:

    bisque-sync render customer_at_risk_reminder      # preview HTML
    bisque-sync plan                                  # show pending changes
    bisque-sync apply --dry-run                       # print without calls
