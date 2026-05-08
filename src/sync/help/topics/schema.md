bisque-sync schema
==================

`bisque-sync schema <provider> [<kind>]` emits the JSON Schema for a
managed resource's YAML shape. Validate YAML against this schema before
writing it.

Examples:

    # List supported kinds for a provider
    bisque-sync schema klaviyo
      -> ["template"]

    # Emit the schema for a specific kind
    bisque-sync schema klaviyo template

    # Validate a YAML file against the schema (requires ajv-cli or similar)
    bisque-sync schema klaviyo template > /tmp/t.json
    ajv validate -s /tmp/t.json -d integrations/klaviyo/templates/welcome.yaml

Fields common to all kinds:

  - `kind`: a constant string identifying the resource type.
  - Resource-specific fields as described in each schema.
