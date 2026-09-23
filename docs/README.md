# Documentation

Start with the documents that describe Geode **as it works now**:

| Document | Purpose |
|---|---|
| [Project README](../README.md) | Product overview, setup, and repository map |
| [Philosophy](PHILOSOPHY.md) | Product and architecture principles |
| [Architecture](current/architecture.md) | Crate boundaries, runtime ownership, configuration, and failure handling |
| [Data path](current/data-path.md) | Current data flow, correctness rules, and reasons for them |
| [Shell](current/shell.md) | Window ownership, tiles, input, focus, modules, and persistence |
| [Crate READMEs](../crates/geode-data/README.md) | Local module maps and crate contracts |
| [Performance](perf.md) | Measurements, conditions, and known gaps |

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

This index will grow as each subsystem is migrated. Until then, start with its
crate README and the code. Gaps in current documentation should be filled here
rather than sending future readers into the implementation archive.
