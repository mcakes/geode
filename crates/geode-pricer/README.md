# geode-pricer

The pure core of the line pricer. It models a sheet and its edits without a
window, tile, data service, or pricing implementation.

Current behavior and status:
[`docs/current/features.md`](../../docs/current/features.md#pricing-and-the-line-pricer).

## Layout

| Module | Holds |
|---|---|
| `sheet` | Struct-of-arrays rows, packages, inherited shifts, and stable line IDs. |
| `edit` | The one mutation door and undo records. |
| `shorthand`, `template` | Parsing and rendering custom lines and package templates. |
| `columns`, `views` | Column vocabulary, prepared column plans, and cell text. |
| `storage` | Conversion between sheets and document rows. |

The tile and application storage workflow are not built, so this crate is not
registered in the module roster.

## Commands

```sh
cargo test -p geode-pricer
cargo bench -p geode-pricer
```

## Rules this crate pins

- Every edit passes through `Sheet::apply`, which returns the undo operation.
- Package rows derive from their legs; they are not independent instruments.
- Shorthand rendering uses a template only while the legs still match it.
- Storage conversion preserves stable ordering and explicit ownership of
  inherited versus row-level shifts.
