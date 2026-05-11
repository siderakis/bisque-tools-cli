# bisque-sync: klaviyo provider

bisque-sync manages Klaviyo resources declaratively. The MVP supports
template resources only. Flows, segments, campaigns, and lists are
deferred.

Layout:

    integrations/klaviyo/
      provider.yaml
      templates/
        welcome.yaml
        customer-at-risk-reminder.yaml
        ...

Supported kinds:

- template (email/SMS template; see `bisque-sync help klaviyo template`)

Supported backend tools (reached via `bisque call`):

- klaviyo_get_templates
- klaviyo_create_template
- klaviyo_update_template

Auth:

Uses your current bisque profile (~/.bisque/config.json). The Klaviyo
connection must be wired up in the workspace's bisque account. Verify
with `bisque connect klaviyo` if a call returns E_AUTH_MISSING.

Examples:

    # One-time bootstrap — pulls every template into YAML
    bisque-sync import klaviyo templates

    # Diff + apply
    bisque-sync plan
    bisque-sync apply
