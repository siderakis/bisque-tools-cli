# Klaviyo Sync Prototype — Plan

## Context

Today, pushing Klaviyo templates requires running a 445-line imperative script (`functions/emails/scripts/push-to-klaviyo.tsx`) with a hand-maintained `TEMPLATES[]` array and a `klaviyo-manifest.json` that conflates remote IDs with project config. Adding or editing a template means three coupled changes, there is no `plan` step, no drift detection, and operational gotchas (template updates don't propagate into flows unless you `--delete-first`) live in tribal knowledge. The L-014 learning in the marketing repo names the core symptom: *copy in repo is not shipped until Klaviyo enabled*.

This plan builds a **minimum declarative sync prototype** inside the existing Rust CLI at `apps/bisque-tools-cli`. A user edits a YAML file (or the TSX it points at), runs `bisque-sync plan`, then `bisque-sync apply`. The CLI resolves desired state against a local state DB, renders HTML via an out-of-process command, and drives the existing `bisque call klaviyo_*` backend tools. Success = `bisque-sync import klaviyo templates` produces 37 YAML files, a subsequent `bisque-sync plan` shows 37 NOOPs, editing one TSX shows exactly 1 UPDATE in the next plan, and `bisque-sync apply` pushes it.

Scope is intentionally narrow: **Klaviyo templates only**. Flows, segments, cross-refs, watch mode, environments, encryption, and metrics sync are explicitly deferred. Metrics remain on-demand via existing `bisque call` tools.

## Agent-first principle

The primary user of `bisque-sync` is an LLM agent; humans are secondary. Every surface decision privileges **zero-guessing operation**: an agent dropped into a repo with a `bisque.yaml` it has never seen should be able to orient, edit, plan, and apply without a web search, a README, or tribal knowledge. This is not a retrofit — it shapes which commands exist, how they output, and how errors read.

Five layered delivery channels, all shipped in the prototype unless noted:

1. **Structured output on every command.** Every verb supports `--json`, and every JSON response follows a documented shape (`{ok, data, error}`). Errors carry `code`, `message`, `remediation`, and `details` fields. Agents parse reliably; humans get the colorized default.

2. **Introspection verbs.** `bisque-sync explain`, `bisque-sync schema`, `bisque-sync ls`, `bisque-sync doctor`, `bisque-sync help <topic>` — purpose-built for agents orienting cold. Described below under "Agent-facing commands."

3. **Workspace CLAUDE.md integration.** `bisque-sync init` writes (or appends to) `CLAUDE.md` in the workspace root a standard orientation stanza: what bisque-sync is, where the files live, the plan→apply workflow, which directories are agent-editable (`integrations/`) and which are not (`.bisque/`). Any agent running in the workspace picks this up automatically.

4. **Claude Code skill.** A `skills/bisque-sync/SKILL.md` file shipped alongside the CLI (in this monorepo), with frontmatter triggering on `bisque.yaml` presence or prompts like "sync klaviyo templates" / "apply bisque changes." The skill body is a concrete workflow walkthrough. Users on Claude Code get it via `/plugin` or a symlink into `~/.claude/skills`.

5. **MCP server** — **verb reserved, implementation deferred.** `bisque-sync mcp` will expose plan/apply/import/render/ls/explain as MCP tools over stdio. Out of prototype scope to build, but the command is reserved in the enum so agents discover it exists and the implementation slot is obvious.

Trigger to flip deferred → built: when the first real agent flow exists that would materially benefit from tool-level MCP access (e.g. an agent iterating plan→apply in a loop within Claude Code) rather than shelling out to CLI.

## Scope

**In:**

- Workspace-aware CLI that reads `bisque.yaml` + `integrations/klaviyo/templates/*.yaml` from the workspace root.
- Local SQLite state DB at `.bisque/state.db`.
- New `bisque-sync` identity (symlink to `bisque`) with: `init`, `import`, `plan`, `apply`, `render`, `explain`, `ls`, `schema`, `doctor`, `help <topic>`. Plus the reserved `mcp` verb.
- Generic `exec` renderer invoked as a subprocess, captures stdout as rendered bytes.
- Mapping YAML template resource → existing `klaviyo_update_template` / `klaviyo_create_template` / `klaviyo_get_templates` backend tools.
- Every command supports `--json`; errors carry `code` / `message` / `remediation` / `details`.
- `skills/bisque-sync/SKILL.md` shipped in this monorepo.
- `bisque-sync init` appends a standard orientation stanza to the workspace `CLAUDE.md`.
- Target workspace: `/Users/siderakis/code/superveggie` (the live repo with 37 templates).
- One small new script in superveggie: `functions/emails/scripts/render-one.tsx` — takes `<tsx-path> <export>`, prints HTML to stdout.

**Out of this prototype:**

- Flows, segments, campaigns, lists.
- Drift detection / `bisque-sync refresh` (deferred; for now the state DB is updated only by `import` and `apply`).
- Cross-integration refs (`${ref.*}`) and reference graph.
- Environments / overlays, `bisque.lock`, secrets encryption at rest.
- Watch mode, daemon mode, file watcher.
- Metrics / observed reports (stay live via `bisque call`).
- Renderer plugins beyond `exec` (no react-email-specific Rust path).
- Parallel applies, apply transactions beyond single-row commits.
- **MCP server implementation** — verb reserved (`bisque-sync mcp`) but returns "not yet implemented" in the prototype.

## Workspace layout (targets `superveggie/`)

```
superveggie/
├── bisque.yaml                              ← NEW, workspace root marker
├── .bisque/
│   └── state.db                             ← NEW, sqlite, gitignored
├── integrations/
│   └── klaviyo/
│       ├── provider.yaml                    ← NEW
│       └── templates/
│           ├── welcome.yaml                 ← NEW × 37
│           ├── customer-at-risk-reminder.yaml
│           └── … (one per current TEMPLATES[] entry)
└── functions/emails/
    ├── scripts/
    │   └── render-one.tsx                   ← NEW, tiny bun script
    └── src/emails/…                          ← UNCHANGED, stays the content source
```

`bisque.yaml` (workspace root):

```yaml
version: 1
name: superveggie
# MVP: no provider versions, no environments.
```

`integrations/klaviyo/provider.yaml`:

```yaml
provider: klaviyo
# MVP: auth comes from existing ~/.bisque/config.json profile.
# No rate_limit (backend enforces), no defaults, no config block.
```

`integrations/klaviyo/templates/customer-at-risk-reminder.yaml` (representative):

```yaml
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
```

`functions/emails/scripts/render-one.tsx` (new in superveggie; ~30 lines):

- Dynamic-import the TSX file, pick the named export, call `@react-email/render`, print HTML to stdout.
- Inject `firstName='{{ first_name|default:"Friend" }}'` (matches today's `KLAVIYO_FIRST_NAME` constant in `push-to-klaviyo.tsx:74`).

## CLI changes (`apps/bisque-tools-cli`)

**One binary, two identities.** The same executable is installed twice: as `bisque` (existing RPC client) and as `bisque-sync` (new reconcile tool). `main.rs` inspects `argv[0]` and dispatches to one of two completely separate `Command` enums, so `bisque --help` continues to show only `login / call / connect / …` and `bisque-sync --help` shows only `init / import / plan / apply / render`. No subcommand overlap, no `init` overload, no mental-model mixing.

Shared underneath: the same `ApiClient`, the same `~/.bisque/config.json` profile, the same `--profile` / `--base-url` / `--api-key` globals.

### Cargo.toml — add deps

```toml
rusqlite    = { version = "0.31", features = ["bundled"] }   # adds ~1.5 MB
serde_yaml  = "0.9"                                           # ~150 KB
walkdir     = "2"                                             # ~50 KB
sha2        = "0.10"                                          # ~80 KB
```

No `jsonschema` yet — MVP does Rust-struct-level validation via `serde_yaml`. No `notify` — no watch mode.

### New modules

```
apps/bisque-tools-cli/src/
├── main.rs                          ← MODIFY: argv[0] detection, two Cli parsers
├── commands.rs                      ← UNCHANGED: handles bisque-identity commands only
├── commands_sync.rs                 ← NEW: handles bisque-sync-identity commands
├── api.rs                           ← UNCHANGED, reused by both identities
├── config.rs                        ← UNCHANGED, reused by both identities
└── sync/                            ← NEW module tree (infrastructure for bisque-sync)
    ├── mod.rs                       ← re-exports, workspace root detection
    ├── workspace.rs                 ← load bisque.yaml + integrations/**
    ├── state.rs                     ← sqlite schema + CRUD
    ├── render.rs                    ← exec renderer (Command::new, capture stdout)
    ├── plan.rs                      ← diff desired vs current → Action enum
    ├── apply.rs                     ← execute plan, update state.db
    └── providers/
        └── klaviyo.rs               ← YAML template ↔ klaviyo_* tool calls, import logic
```

### `main.rs` restructure

```rust
fn main() {
    let argv0 = std::env::args().next().unwrap_or_default();
    let is_sync = std::path::Path::new(&argv0)
        .file_name()
        .map(|n| n == "bisque-sync")
        .unwrap_or(false);

    let result = if is_sync {
        let cli = SyncCli::parse();
        commands_sync::run(cli)
    } else {
        let cli = Cli::parse();             // existing
        commands::run(cli)
    };

    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

// Existing enum — NO new variants added; unchanged surface.
#[derive(Parser)]
#[command(name = "bisque", version, about = "Bisque CLI — manage integrations and execute tools")]
pub struct Cli { /* ... unchanged ... */ }

// New, parallel enum — only for bisque-sync invocations.
#[derive(Parser)]
#[command(name = "bisque-sync", version,
          about = "Bisque Sync — declarative project state for SaaS integrations")]
pub struct SyncCli {
    #[command(subcommand)]
    pub command: SyncCommand,

    // Same globals as Cli (reuse the same flag layer).
    #[arg(long, global = true)] pub profile: Option<String>,
    #[arg(long, global = true)] pub base_url: Option<String>,
    #[arg(long, global = true)] pub user_id: Option<String>,
    #[arg(long, global = true)] pub api_key: Option<String>,
    #[arg(long, global = true)] pub pretty: bool,
    #[arg(long, global = true)] pub summary_only: bool,
    #[arg(long, global = true)] pub field: Option<String>,
}

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Scaffold bisque.yaml + .bisque/ + CLAUDE.md stanza in the current directory
    Init {
        /// Do not write/append to CLAUDE.md
        #[arg(long)] no_claude_md: bool,
    },

    /// Import remote resources into YAML + state.db
    Import {
        /// Provider name (e.g. klaviyo)
        provider: String,
        /// Resource kind (e.g. templates). Defaults to all kinds the provider supports.
        kind: Option<String>,
    },

    /// Show pending changes
    Plan,

    /// Apply pending changes
    Apply {
        /// Print what would be called, skip the actual API calls
        #[arg(long)] dry_run: bool,
        /// Skip interactive confirmation (required when stdin is not a TTY)
        #[arg(long)] auto_approve: bool,
    },

    /// Render a managed resource (preview output bytes without applying)
    Render { resource: String },

    // ─── Agent-first introspection ───

    /// Print a workspace snapshot (providers, resources, state, pending plan summary)
    Explain,

    /// List managed resources with current/desired state
    Ls {
        /// Filter by provider (e.g. klaviyo)
        provider: Option<String>,
        /// Filter by kind (e.g. templates)
        kind: Option<String>,
    },

    /// Print the JSON Schema for a resource kind's YAML shape
    Schema {
        /// Provider name (e.g. klaviyo)
        provider: String,
        /// Resource kind (e.g. template). If omitted, lists available kinds for the provider.
        kind: Option<String>,
    },

    /// Verify workspace integrity, auth, render dependencies, known quirks
    Doctor,

    /// Print a help topic: workflow | schema | troubleshooting | <provider> | <provider> <kind>
    Help { topic: Option<Vec<String>> },

    /// RESERVED — MCP server over stdio (returns E_NOT_IMPLEMENTED in prototype)
    Mcp,
}
```

Existing `Command::Init` stays untouched. There is no `bisque sync` subcommand — the sync verbs are only reachable via the `bisque-sync` identity.

### Agent-facing command shapes

**`bisque-sync explain`** — one-shot orientation. Output includes: workspace root path, providers configured, count of managed resources per kind, state.db health (exists, writable, row count), pending plan summary (creates/updates/noops), last apply timestamp, next suggested command. The agent's first command after landing in an unfamiliar repo. `--json` returns this structured.

**`bisque-sync schema <provider> [<kind>]`** — emits the JSON Schema for the YAML shape. Without `<kind>`, lists supported kinds. Agents validate YAML before writing. Keeps schemas co-located with the provider code in Rust (source of truth), emitted deterministically.

**`bisque-sync ls [<provider>] [<kind>]`** — table of managed resources with columns: name, kind, remote_id (or `—`), status (`noop|pending|untracked|orphaned`), file_path, last_applied. `--json` returns an array. Default sort is by provider, kind, name.

**`bisque-sync doctor`** — pre-flight checks. Verifies: `bisque.yaml` present and parseable; `.bisque/state.db` writable; auth profile resolves and passes a ping tool call; each configured provider's required tools are reachable; render commands referenced in YAML are executable. Per-check output: `PASS`, `WARN`, or `FAIL: <remediation>`. Exits non-zero on any FAIL.

**`bisque-sync help <topic>`** — rich in-terminal docs. Topics in prototype: `workflow` (end-to-end walkthrough), `schema` (meta-guide to reading schema output), `troubleshooting` (common errors + codes), `klaviyo` (provider overview), `klaviyo template` (kind overview with an example YAML block). Each topic ends with at least two concrete command examples copy-pasteable as-is.

**Error envelope (every command):**
```json
{
  "ok": false,
  "error": {
    "code": "E_RENDER_FAILED",
    "message": "Rendering template 'customer-at-risk-reminder' failed with exit code 2.",
    "remediation": "Run `bun functions/emails/scripts/render-one.tsx --export ReminderEmail <path>` manually to see the underlying error.",
    "details": {
      "resource": "klaviyo.template.customer_at_risk_reminder",
      "file": "integrations/klaviyo/templates/customer-at-risk-reminder.yaml",
      "command": ["bun", "functions/emails/scripts/render-one.tsx", "--export", "ReminderEmail", "..."],
      "exit_code": 2,
      "stderr_tail": "ReferenceError: ..."
    }
  }
}
```

Stable codes for the prototype: `E_NO_WORKSPACE`, `E_YAML_PARSE`, `E_SCHEMA_VIOLATION`, `E_RENDER_FAILED`, `E_AUTH_MISSING`, `E_TOOL_CALL_FAILED`, `E_REMOTE_NOT_FOUND`, `E_STATE_DB`, `E_NOT_IMPLEMENTED`.

### Skill and CLAUDE.md stanza

**`skills/bisque-sync/SKILL.md`** (new, in this monorepo):

- Frontmatter:
  ```
  ---
  name: bisque-sync
  description: Use when the workspace has a bisque.yaml file OR the user wants to declaratively manage Klaviyo (or other supported) resources as files. Handles plan/apply workflows, YAML schema, common errors.
  ---
  ```
- Body: 3–5 screens of workflow guidance with concrete command sequences, expected output shapes, and error-code reference. Written as an agent would need to read it, not as prose.

**Workspace CLAUDE.md stanza** (written by `bisque-sync init` unless `--no-claude-md`):

```markdown
## bisque-sync

This workspace uses bisque-sync for declarative SaaS state management.

- Desired state: `integrations/<provider>/<kind>/*.yaml` — edit freely.
- Source content referenced by YAML (e.g. TSX templates): edit freely.
- Runtime state: `.bisque/state.db` — never edit directly; managed by CLI.
- Workflow: edit YAML or referenced sources → `bisque-sync plan` → `bisque-sync apply`.
- Orient: `bisque-sync explain` prints workspace state + next suggested command.
- Validate before writing YAML: `bisque-sync schema <provider> <kind>`.
- All commands accept `--json` for structured output.
```

### Install / release wiring

- **`apps/bisque-tools-cli/install.sh`** — after placing the binary, create a symlink `bisque-sync → bisque` in the same install dir:

  ```sh
  ln -sf bisque "$INSTALL_DIR/bisque-sync"
  ```

- **`apps/bisque-tools-cli/release.sh`** — no change. Same single binary ships; the symlink is a local install step.

- **GitHub Actions release workflow** — no change to the build matrix. The tarball still contains one binary; `install.sh` creates the alias on the user's machine.

- **Uninstall** — extend the existing uninstall path (if any) to also remove the symlink.

### State DB schema (`sync/state.rs`)

```sql
CREATE TABLE resources (
    provider       TEXT    NOT NULL,    -- "klaviyo"
    kind           TEXT    NOT NULL,    -- "template"
    name           TEXT    NOT NULL,    -- slug from filename, e.g. "customer_at_risk_reminder"
    file_path      TEXT    NOT NULL,    -- relative to workspace root
    remote_id      TEXT,                -- Klaviyo template ID (null if never applied)
    desired_hash   TEXT    NOT NULL,    -- sha256 of (yaml canonical + rendered html)
    applied_hash   TEXT,                -- hash at last successful apply
    last_applied   INTEGER,             -- unix seconds
    PRIMARY KEY (provider, kind, name)
);

CREATE TABLE apply_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    provider      TEXT    NOT NULL,
    kind          TEXT    NOT NULL,
    name          TEXT    NOT NULL,
    action        TEXT    NOT NULL,   -- "create" | "update" | "noop"
    started_at    INTEGER NOT NULL,
    finished_at   INTEGER,
    outcome       TEXT,               -- "success" | "failed"
    error         TEXT,
    remote_id     TEXT
);
```

One row per managed template. No pending_ops table yet (no crash recovery in MVP; failed apply just re-diffs on next plan).

### Plan algorithm (`sync/plan.rs`)

For each template YAML in `integrations/klaviyo/templates/*.yaml`:

1. Parse YAML → `TemplateResource { name, klaviyo_name, html_spec }`.
2. Render HTML by executing `html_spec.command` in workspace root; capture stdout.
3. Compute `desired_hash = sha256(canonical_yaml_bytes || rendered_html_bytes)`.
4. Look up state row by `(klaviyo, template, name)`.
5. Decide action:
   - No row → **CREATE**.
   - `desired_hash == applied_hash` → **NOOP**.
   - Otherwise → **UPDATE**.
6. Accumulate into `Plan { creates: […], updates: […], noops: […] }`.

Render is parallelized with a bounded semaphore (8 at a time) to keep 37 TSX renders fast.

### Apply (`sync/apply.rs`)

For each planned action, in file-name order:

- **CREATE:** call `klaviyo_create_template` with `{ data: { type: "template", attributes: { name, editor_type: "CODE", html } } }`; parse `data.data.id`; INSERT state row with `remote_id`.
- **UPDATE:** call `klaviyo_update_template` with `{ id: remote_id, data: { type: "template", id: remote_id, attributes: { name, html } } }`. On failure with status suggesting 404, fall back to CREATE (same behavior as today's `push-to-klaviyo.tsx:381-403`).
- After each call: write apply_log row, update `applied_hash` + `last_applied` in resources.
- Rate limit: the backend already enforces Klaviyo limits; we add no client-side sleep.
- On any non-recoverable error, stop and report; already-applied rows stay applied.

### Import (`sync/providers/klaviyo.rs`)

`bisque-sync import klaviyo templates` is the bootstrap. Steps:

1. `api::post_tool_call("klaviyo_get_templates", {})` — pagination handled via whatever the tool exposes (inspect response; if truncated, iterate with cursor).
2. Optionally use `superveggie/functions/emails/klaviyo-manifest.json` to preserve existing slug → ID mapping (so filenames stay stable). Read, don't write.
3. For each remote template:
   - Slug = `filenameToKey(name)` analog; fallback to sanitized remote name.
   - Write `integrations/klaviyo/templates/<slug>.yaml`:
     - `name:` from remote attributes.
     - `html.render: exec` with a placeholder command pointing to the matching TSX export *if* the slug matches an entry in today's manifest; otherwise a comment instructing the user to point at a source.
   - INSERT state row: `remote_id = remote.id`, `desired_hash = applied_hash = <hash of current render, or null if source unknown>`.
4. Print summary: `Imported 37 templates, 37 mapped to existing TSX sources, 0 require manual source binding.`

Deliberate: import does *not* try to diff remote HTML against a re-rendered local HTML. The first post-import `bisque-sync plan` will show changes only for templates whose re-render differs from what was last pushed — which is exactly the useful signal (pending work).

### Reused utilities (do not re-implement)

| Need | Reuse from |
|---|---|
| Auth, profile resolution | `config::require_auth`, `config::resolve_profile_name` (config.rs) |
| HTTP tool call | `ApiClient::post_tool_call` (api.rs:47) |
| Response parsing | `ToolCallResponse` enum (api.rs:6) |
| JSON shape for `klaviyo_update_template` / `klaviyo_create_template` | mirror `superveggie/functions/emails/scripts/push-to-klaviyo.tsx:385-412` |
| Slug derivation from filename | mirror `superveggie/functions/emails/scripts/lib/manifest.ts::filenameToKey` |
| Where to read Klaviyo IDs during first import | `superveggie/functions/emails/klaviyo-manifest.json` (read-only) |

## End-to-end verification

Run in order against the live superveggie workspace (all sync verbs use the `bisque-sync` identity, which is a symlink to the same binary):

1. `cd /Users/siderakis/code/superveggie`
2. `bisque-sync init` → creates `bisque.yaml`, `.bisque/`, `.gitignore` entry for `.bisque/state.db`.
3. Hand-create `integrations/klaviyo/provider.yaml` (or let `bisque-sync init --provider klaviyo` do it; either works in MVP).
4. Add `functions/emails/scripts/render-one.tsx` (tiny new file).
5. `bisque-sync import klaviyo templates` → expect:
   - `integrations/klaviyo/templates/*.yaml` × ~37 files created.
   - `.bisque/state.db` has 37 rows in `resources`.
   - Each YAML points at the correct TSX export (matched via existing manifest).
6. `bisque-sync plan` → expect **0 changes** (or a small number for templates whose TSX has drifted since last push-to-klaviyo).
7. Edit `functions/emails/src/emails/segments/customer-at-risk/1-reminder.tsx` (change one word).
8. `bisque-sync plan` → expect exactly **1 UPDATE** for `customer_at_risk_reminder`, with a byte delta in the output.
9. `bisque-sync apply --dry-run` → prints what it *would* call, does not hit the API.
10. `bisque-sync apply` → prompts for confirmation (TTY), calls `klaviyo_update_template`, reports success, updates `applied_hash` in state.db.
11. Re-run `bisque-sync plan` → expect **0 changes** again.
12. Revert the edit, `bisque-sync plan` → **1 UPDATE** (reverse direction). Apply it.

Identity sanity checks:

- `bisque --help` → shows only the existing verbs; no mention of plan/apply/import/render.
- `bisque-sync --help` → shows only sync verbs (init/import/plan/apply/render/explain/ls/schema/doctor/help/mcp).
- `bisque --version` and `bisque-sync --version` report the same string.
- `readlink $(which bisque-sync)` resolves to `bisque`.

Agent-first checks:

- `bisque-sync explain` in a fresh workspace → structured snapshot; `--json` output matches a fixed shape.
- `bisque-sync schema klaviyo template` → prints a valid JSON Schema that an agent could feed to a JSON-Schema validator.
- `bisque-sync help workflow` → prints a multi-step walkthrough ending with at least two concrete command examples.
- Induce a render failure (rename `render-one.tsx`) → `bisque-sync plan --json` returns `{ok: false, error: {code: "E_RENDER_FAILED", remediation: "...", details: {...}}}`.
- `bisque-sync init` → creates `bisque.yaml`, `.bisque/`, and appends the documented stanza to workspace `CLAUDE.md`.
- `bisque-sync init --no-claude-md` → same, without touching `CLAUDE.md`.
- `bisque-sync mcp` → exits with `E_NOT_IMPLEMENTED`, prints a remediation explaining CLI is the current interface.

Negative tests:

- Break one template YAML (bad key) → `bisque-sync plan` reports the file + line with a serde error, doesn't crash.
- Delete `.bisque/state.db` → `bisque-sync plan` reports all 37 as CREATE. Don't apply. Re-run `bisque-sync import klaviyo templates` to rebuild state cleanly.
- Unset bisque auth → `bisque-sync plan` fails with the same auth error path as `bisque call`.
- Pipe `bisque-sync apply` with no `--auto-approve` → fails clearly (non-TTY requires explicit flag).

## Critical files

**To create (CLI):**

- `apps/bisque-tools-cli/src/commands_sync.rs` — dispatch for `bisque-sync` identity.
- `apps/bisque-tools-cli/src/sync/mod.rs`
- `apps/bisque-tools-cli/src/sync/workspace.rs`
- `apps/bisque-tools-cli/src/sync/state.rs`
- `apps/bisque-tools-cli/src/sync/render.rs`
- `apps/bisque-tools-cli/src/sync/plan.rs`
- `apps/bisque-tools-cli/src/sync/apply.rs`
- `apps/bisque-tools-cli/src/sync/errors.rs` — error-envelope types, stable codes, JSON serialization.
- `apps/bisque-tools-cli/src/sync/help/mod.rs` — help topic registry + topic rendering.
- `apps/bisque-tools-cli/src/sync/help/topics/` — one file per topic: `workflow.md`, `schema.md`, `troubleshooting.md`, `klaviyo.md`, `klaviyo_template.md` (embedded at compile time via `include_str!`).
- `apps/bisque-tools-cli/src/sync/providers/mod.rs`
- `apps/bisque-tools-cli/src/sync/providers/klaviyo.rs`
- `apps/bisque-tools-cli/src/sync/providers/klaviyo_schemas/` — JSON Schema files (e.g. `template.schema.json`) embedded via `include_str!`.

**To create (monorepo, outside CLI crate):**

- `skills/bisque-sync/SKILL.md` — Claude Code skill; frontmatter triggers on `bisque.yaml` presence or sync-related prompts.

**To modify (CLI):**

- `apps/bisque-tools-cli/Cargo.toml` — add rusqlite, serde_yaml, walkdir, sha2.
- `apps/bisque-tools-cli/src/main.rs` — add `argv[0]` detection, add `SyncCli` / `SyncCommand` enums, branch dispatch to `commands` vs `commands_sync`.
- `apps/bisque-tools-cli/src/commands.rs` — **unchanged** (no new variants to handle — bisque identity stays as-is).
- `apps/bisque-tools-cli/install.sh` — add `ln -sf bisque "$INSTALL_DIR/bisque-sync"` after binary install.

**To create (superveggie workspace):**

- `bisque.yaml`
- `integrations/klaviyo/provider.yaml`
- `integrations/klaviyo/templates/*.yaml` (37, generated by `bisque-sync import klaviyo templates`)
- `functions/emails/scripts/render-one.tsx` (one-off render wrapper)
- `.gitignore` entry for `.bisque/`

## Deferred (non-goals — explicit, so we don't scope-creep)

- Flows, segments, campaigns, lists — next iteration.
- Cross-integration refs, dependency graph — needed once flows arrive, not before.
- `bisque-sync refresh` (remote drift detection) — the gap that lets Klaviyo UI edits diverge silently. High value, but distinct work.
- Renderer plugins (react-email, mjml, html-passthrough) as first-class Rust modules. The generic `exec` renderer covers everything for now.
- State DB encryption (SQLCipher) — state contains IDs, not secrets, at this stage.
- Cursor-based / incremental import.
- Watch mode, daemon, auto-apply.
- Metrics sync (user confirmed: on-demand via existing `bisque call` is fine).
- Secrets handling beyond existing `~/.bisque/config.json` profile auth.

---

# Appendix: Design context (for resuming on another machine)

This appendix captures the conversation that produced the plan above, so a future session — or a future machine — can pick up without re-deriving every decision.

## Problem statement (original prompt)

> I want to create a system that models and syncs data between my local filesystem and cloud services (SaaS) like Klaviyo or Meta Ads, etc. ads/campaigns/etc are defined locally and get synced to the service automatically. validated when? also metrics etc flow back to the file system. make all data local. it's for AI agents to work, like MCP etc.

## Design decisions & rationale

Ordered by when they were made, with the reasoning and — where relevant — what was rejected.

1. **Reference architecture: Infrastructure-as-Code (Terraform / Flux).** Maps almost 1:1. Key concepts imported: desired state vs. actual state, providers-as-adapters, refresh for drift detection, plan/apply, resource graphs, import for bootstrapping.

2. **Language: Rust.** Already committed — the existing CLI at `apps/bisque-tools-cli` is Rust. Tradeoff accepted: weaker SaaS SDK ecosystem than TS/Python, harder schema evolution. Upsides: single static binary, strong parsing, fast I/O.

3. **Not a gRPC plugin protocol (abandoned mid-conversation).** Initial direction was Terraform-style: plugins as separate processes communicating via gRPC (`GetSchema`, `Plan`, `Apply`). Rejected once we realized the **bisque backend is already a plugin host** — `bisque call klaviyo_update_template {…}` works today. So "provider plugins" are just namespaced sets of backend tools the CLI orchestrates. No new process model.

4. **Renderers ≠ providers.** Rendering email TSX to HTML is a separate concern. Recognized as an "asset pipeline" step. Subprocess-based `exec` renderer as universal escape hatch covers react-email, MJML, Maizzle, HTML passthrough, etc. without any per-framework Rust code in MVP.

5. **Renderer plugin protocol (designed but deferred).** Proto for a `Renderer` service with `Render(source, inputs) → (output, content_hash, dependencies[], warnings[])`. The `dependencies` field is load-bearing for file-watch invalidation. Not built in MVP — `exec` is enough.

6. **Config is decomposed, not monolithic.** At 20 integrations one big `bisque.yaml` is unreadable. Decision: thin root `bisque.yaml`, one directory per integration at `integrations/<provider>/`, one resource per file under `<kind>/`. Convention-based discovery (no central registration). Mirrors dbt's `dbt_project.yml` + models, Kustomize overlays.

7. **State vs. config separation.** Remote IDs go in `.bisque/state.db` (SQLite). Never in YAML, never in git. Workspace YAML stays ID-free so files are portable across environments. Fixes the current `klaviyo-manifest.json` problem of conflating IDs with config.

8. **Metrics: on-demand, not synced.** After evaluation, concluded metrics sync is over-engineered for current use. Live MCP/API pull handles 80% of agent queries. Revisit when a *specific* cross-provider join or historical-preservation query emerges. Managed-state sync has clearer ROI (fixes L-014).

9. **Local data storage primer (for reference, mostly unused in MVP).** Evaluated SQLite vs DuckDB vs Parquet vs JSONL. Decision for MVP: SQLite only (state DB). Parquet/DuckDB would only matter for metrics sync, which is deferred.

10. **Two identities, one binary.** The existing `bisque` is a thin RPC client; the new sync work is a stateful reconcile tool. Rather than bloat the command tree with mixed concerns, install the binary twice via symlink — as `bisque` and `bisque-sync` — with `main.rs` inspecting `argv[0]` and dispatching to separate `Command` enums. No subcommand overlap (`bisque-sync init ≠ bisque init`).

11. **Rejected: separate binary (`apps/bisque-sync/`).** Considered and deferred. Binary size concern dismissed after checking: current `bisque` is 1.8 MB; with rusqlite + serde_yaml + walkdir + sha2 it goes to ~4–5 MB. Not a user-facing issue. Stay single binary until sync surface stabilizes; then split if it outgrows.

12. **Rejected: `bisque sync <verb>` subcommand namespace.** Considered. Cleaner to ship same-binary but present as two tools via argv[0] tricks. Users get `bisque-sync plan`, not `bisque sync plan`. If we ever split to a real second binary, users type the same command; zero UX churn.

13. **Agent-first, not retrofitted.** Added as a top-level design principle when user flagged that "the agent is our user." Five layered channels: structured `--json` output, introspection verbs, CLAUDE.md stanza written by `init`, Claude Code skill file, reserved `mcp` verb. Not a nice-to-have — it reshapes the command tree.

14. **MVP scope lock:** Klaviyo templates only. Everything else (flows, segments, refresh, cross-refs, watch, environments, encryption, MCP implementation) is explicitly deferred and listed as "out of scope."

## Rejected alternatives (quick reference)

| Rejected | In favor of | Because |
|---|---|---|
| Bidirectional sync | One-way (files → remote, state.db cached back) | Conflict resolution is where bidirectional systems collapse. Drift detection via `refresh` is simpler. |
| HCL / CUE / Starlark as config format | YAML + JSON Schema | Ecosystem and agent familiarity. CUE is interesting for v2. |
| Dedicated react-email renderer in Rust | `exec` renderer + workspace script | Framework-agnostic. No Node bundler in Rust. |
| Terraform-style gRPC provider plugins | Backend tools as providers | Backend already does the SaaS talking; no new plugin runtime to invent. |
| Separate `apps/bisque-sync/` crate | `argv[0]` multi-call binary | Same mental separation, half the release/install work. |
| `bisque sync <verb>` namespace | `bisque-sync <verb>` symlink | Cleaner `--help`, cleaner mental model. |
| Synced metrics store (Parquet + DuckDB) | Live API via existing `bisque call` | ROI not proven; managed state is where the pain is. |
| Auto-apply on file save | Explicit `plan` → `apply` | Destructive operations need gates. |

## Target workspace (for prototype)

**`/Users/siderakis/code/superveggie`** — monorepo containing the Super Veggie Delivery marketing infrastructure. Has 37 Klaviyo templates under `functions/emails/src/emails/`, currently pushed by an imperative script. This is where `bisque-sync` runs end-to-end.

## Relevant existing files (with purpose)

### In this monorepo (`execution.run`)

- `apps/bisque-tools-cli/src/main.rs` — CLI entrypoint, clap `Cli` / `Command` enums.
- `apps/bisque-tools-cli/src/commands.rs` — existing command dispatch (~1500 lines). Will not be modified.
- `apps/bisque-tools-cli/src/api.rs` — `ApiClient` with `post_tool_call` (api.rs:47). This is the **reuse point** for sync apply.
- `apps/bisque-tools-cli/src/config.rs` — profile resolution, auth (`config::require_auth`). Reused.
- `apps/bisque-tools-cli/Cargo.toml` — current deps: anyhow, clap, dirs, serde, serde_json, open, ureq, flate2, hostname, tar. Add: rusqlite, serde_yaml, walkdir, sha2.
- `apps/bisque-tools-cli/install.sh` — user install script; will get the `ln -sf bisque bisque-sync` line.
- `apps/bisque-tools-cli/release.sh` — tag + subtree-push → triggers GH Actions. No change needed.
- `packages/tool-catalog/src/providers/klaviyo.ts` — source of truth for Klaviyo tool names (e.g. `klaviyo_get_templates`, `klaviyo_update_template`, `klaviyo_create_template`). Skim for exact parameter shapes when implementing `providers/klaviyo.rs`.
- `functions/bisque/src/klaviyo/types.ts` — Klaviyo type defs on the backend.

### In target workspace (`superveggie`)

- `functions/emails/scripts/push-to-klaviyo.tsx` — the 445-line imperative script this replaces. Reference for:
  - Line 74 — `KLAVIYO_FIRST_NAME` constant (placeholder value injected at render time).
  - Lines 76–324 — `TEMPLATES[]` array (37 entries). One row per template we'll manage.
  - Lines 381–403 — `update → fall back to create` pattern that the sync apply loop must replicate.
- `functions/emails/scripts/lib/bisque.ts` — thin wrapper showing how TS shells out to `bisque call <tool>`. Confirms the tool-call I/O pattern.
- `functions/emails/scripts/lib/manifest.ts` — `filenameToKey` logic for slug derivation; mirror in Rust for stable naming.
- `functions/emails/klaviyo-manifest.json` — existing state file. Contains current Klaviyo IDs (templates, flows, segments). Read-only during import — used to map remote IDs to TSX source files without having to round-trip everything.
- `functions/emails/src/emails/segments/customer-at-risk/1-reminder.tsx` — representative template (ReminderEmail export). Use as the end-to-end test edit target.
- `marketing/learnings/L-014-copy-in-repo-is-not-shipped-until-klaviyo-enabled.md` — recorded pain point this prototype addresses.

## Working directories

- Primary CLI code: `/Users/siderakis/code/execution.run/apps/bisque-tools-cli`
- Target workspace for live testing: `/Users/siderakis/code/superveggie`
- Plan file (session-local): `/Users/siderakis/.claude/plans/deep-tinkering-riddle.md`
- Persistent plan (this file): `apps/bisque-tools-cli/docs/klaviyo-sync-prototype-plan.md`

## Suggested implementation order (when resuming)

Dependency order, each stage testable before the next:

1. **Scaffold.** Add Cargo.toml deps; create `sync/` module tree with empty stubs; add `SyncCli` / `SyncCommand` enums in main.rs; add argv[0] detection + dispatch.
2. **`bisque-sync init`.** Simplest verb, no network. Writes `bisque.yaml`, `.bisque/`, `.gitignore`, CLAUDE.md stanza. Test with an empty dir first.
3. **State DB (`sync/state.rs`).** Create schema on first use. CRUD helpers for the `resources` and `apply_log` tables.
4. **Workspace loader (`sync/workspace.rs`).** Discover `bisque.yaml`, walk `integrations/*/provider.yaml`, parse YAML by `kind:` per resource file. Unit-testable without network.
5. **`bisque-sync explain`.** Reads workspace + state.db, prints orientation. No network. Agent-first sanity check.
6. **`bisque-sync schema klaviyo template`.** Embed JSON Schema via `include_str!`, print. Static, no state.
7. **Render pipeline (`sync/render.rs`).** Given a `render: exec` spec, run the command, capture stdout, sha256 it. Unit-test with a trivial command (`echo hello`).
8. **`bisque-sync plan`.** Pulls it together: walk resources → render → hash → diff vs state → print. No network.
9. **`bisque-sync import klaviyo templates`.** First network-touching command. Call `klaviyo_get_templates`, write 37 YAMLs, insert state rows.
10. **`bisque-sync apply`.** Loop plan → for each action, call the right `klaviyo_*` tool via existing `ApiClient::post_tool_call`, update state DB.
11. **Errors.** Wrap everything in the documented `{ok, data, error}` envelope; add `--json` global.
12. **Help topics.** Write prose for the five topics; embed via `include_str!`.
13. **`bisque-sync doctor`, `bisque-sync ls`.** Derivative of workspace + state DB; add last.
14. **`bisque-sync mcp`.** Stub that exits with `E_NOT_IMPLEMENTED`.
15. **install.sh symlink line.** Add `ln -sf bisque "$INSTALL_DIR/bisque-sync"`.
16. **`skills/bisque-sync/SKILL.md`.** Last — written after we know how the tool actually behaves.

## Open questions (not blocking MVP, flagged for later)

- **Template edits inside live flows.** Klaviyo clones template content into flow actions at creation time; updating a library template does NOT update the flow. Today's script uses `--delete-first` on the flow. The prototype only manages templates, so this isn't an MVP issue, but the moment flows land, `bisque-sync plan` must detect "template used in flow changed → flow needs replace" and surface that before destroying anything live. Reserve `--replace-dependent-flows` as a future flag.
- **Drift detection (`bisque-sync refresh`).** Anyone editing a template in the Klaviyo UI causes silent divergence until next apply. High-value v2 feature.
- **State DB encryption.** Not MVP (state contains only IDs), but if we add flows with audience filters or profile references, SQLCipher on `state.db` becomes warranted.
- **Plugin / schema distribution.** If a third party wants to add a provider, today they'd need to modify the Rust crate + the bisque backend. Revisit once a second provider exists.

## Conversation-level instincts worth preserving

- **"We already have the backend as the plugin host"** — the most load-bearing simplification. Anytime the design gets complex, ask: does the backend already do this?
- **"Agent is the user"** — every `--help`, every error message, every output shape should be evaluated from an agent's perspective, not a human's.
- **"Scope is infrastructure" / "If you don't have a problem that needs it, don't build it"** — applied to metrics sync specifically. Resist the urge to generalize before you have the second use case.
- **"YAML per resource, one file per thing"** — filesystem granularity matters for git diffs, file watchers, agent edits. No multi-doc YAML, no "one big config."
- **"Never mix state and config"** — the fix for `klaviyo-manifest.json`. State in `.bisque/state.db`. Config in `integrations/**`. Git sees only config.
