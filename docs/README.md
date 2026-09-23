# Documentation

Start with the documents that describe Geode **as it works now**:

| Document | Purpose |
|---|---|
| [Project README](../README.md) | Product overview, setup, and repository map |
| [Philosophy](PHILOSOPHY.md) | Product and architecture principles |
| [Architecture](current/architecture.md) | Crate boundaries, runtime ownership, configuration, and failure handling |
| [Configuration](current/configuration.md) | Layers, documents, validation, writes, reload, keymaps, theme, time, and logging |
| [Data path](current/data-path.md) | Current data flow, correctness rules, and reasons for them |
| [Requests and UI delivery](current/request-delivery.md) | Request admission, cancellation, reload, shutdown, mailbox coalescing, and window routing |
| [Shell](current/shell.md) | Window ownership, tiles, input, focus, modules, and persistence |
| [Feature modules](current/features.md) | Blotter, documents, timeseries, diagnostics, pricing, and demo behavior |
| [Crate READMEs](../crates/) | Local module maps and crate contracts |
| [Performance](current/performance.md) | Budgets, instrumentation, reference values, and known gaps |
| [Measurement log](perf.md) | Chronological benchmark results and investigations |

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
