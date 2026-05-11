# bisque-sync troubleshooting

Error codes (stable; switch on these in agent code):

E_NO_WORKSPACE No bisque.yaml found. Run `bisque-sync init`.
E_YAML_PARSE A YAML file could not be parsed. The error details
name the file and the parser message.
E_SCHEMA_VIOLATION YAML parsed but failed schema validation.
E_RENDER_FAILED The `html.command` subprocess exited non-zero.
The error `details.stderr_tail` shows the tail.
E_AUTH_MISSING Bisque auth is missing. Run `bisque login`.
E_TOOL_CALL_FAILED A `bisque call <tool>` request failed.
E_REMOTE_NOT_FOUND Apply tried to update a template whose remote ID
no longer exists. Apply falls back to CREATE
automatically, but you should `import` again.
E_STATE_DB Reading or writing `.bisque/state.db` failed.
E_NOT_IMPLEMENTED A verb or provider is a reserved placeholder.

Common fixes:

# "No bisque.yaml found"

cd /path/to/workspace && bisque-sync init

# "E_RENDER_FAILED"

# Re-run the render command manually to see the underlying error.

bun functions/emails/scripts/render-one.tsx --export ... <source.tsx>

# "E_AUTH_MISSING"

bisque login

# Corrupted state DB

rm .bisque/state.db
bisque-sync import klaviyo templates

Examples:

    bisque-sync doctor        # pre-flight checks
    bisque-sync plan --json   # structured output for agent parsing
