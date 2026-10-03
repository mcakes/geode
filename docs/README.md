# Documentation

New users should start with the [user guide](user-guide.md): a guided demo
walkthrough that introduces Geode's philosophy, tiles, shared context, and
everyday interaction patterns.

Start with the documents that describe Geode **as it works now**:

| Document | Purpose |
|---|---|
| [User guide](user-guide.md) | First steps, a guided demo walkthrough, and everyday patterns |
| [Project README](../README.md) | Product overview, setup, and repository map |
| [Philosophy](PHILOSOPHY.md) | Product and architecture principles |
| [Architecture](current/architecture.md) | Crate boundaries, runtime ownership, configuration, and failure handling |
| [Configuration](current/configuration.md) | Layers, documents, validation, writes, reload, keymaps, theme, time, and logging |
| [Typed documents](current/typed-documents.md) | Schema, views, presentation, grouping, scope, colour, and formatting reader contracts |
| [Configuration dialogs](current/configuration-dialogs.md) | Drafts, validation, overrides, immediate application, persistence, and failure behavior |
| [Keymaps and actions](current/keymaps.md) | Binding precedence, predicates, sequences, counts, module fragments, editing, and reset |
| [Data path](current/data-path.md) | Current data flow, correctness rules, and reasons for them |
| [Requests and UI delivery](current/request-delivery.md) | Request admission, cancellation, reload, shutdown, mailbox coalescing, and window routing |
| [Shell](current/shell.md) | Window ownership, tiles, input, focus, modules, and persistence |
| [Input and dialogs](current/input-and-dialogs.md) | Keyboard ownership, modal lifetime, palette, completion, choices, and frame pickers |
| [Tiling and workspaces](current/tiling.md) | Layout geometry, region focus, stacks, transfers, resizing, and restoration limits |
| [Feature modules](current/features.md) | Blotter, documents, timeseries, vol slice, diagnostics, pricing, and demo behavior |
| [Crate READMEs](../crates/) | Local module maps and crate contracts |
| [Performance](current/performance.md) | Budgets, instrumentation, reference values, and known gaps |
| [Measurement log](perf.md) | Chronological benchmark results and investigations |

`modules.md` is an undated feature-ideas inventory, not a current capability
reference or committed roadmap.

`phase-history.md` and `superpowers/` are an archive of proposals,
implementation steps, rulings, and review findings. They may describe
superseded designs and are not part of the normal reading path. Consult the
archive only when the reason for a current contract is still unclear.

## Writing current guides

Describe the behavior and the reason it exists. State invariants, failure
semantics, and known limitations explicitly. Link to the code that enforces
them. Put task sequences, commit instructions, review chronology, and
superseded approaches in the dated record. When implementation changes a
contract, update its current guide in the same change.

Start with the relevant current guide, then its crate README and code. Fill
gaps in the maintained guides rather than sending future readers into the
implementation archive.

Check Rust documentation in both public and internal views:

```sh
cargo doc --workspace --no-deps
cargo doc --workspace --no-deps --document-private-items
```

Resolve broken links and markup warnings in both modes. Public comments
should explain the contract without requiring a link to a private helper.
Check Markdown links against their destination files and headings, and verify
commands, defaults, and limitations against the implementation and tests.
