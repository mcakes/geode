# Getting comfortable with Geode

Geode brings risk, market data, pricing, and charts into one desktop workspace.
You arrange the information around the question you are trying to answer, then
move between views without losing your place.

This guide is for someone joining a desk that uses Geode. You do not need to
know Vim, write configuration files, or understand how Geode is built. The
walkthrough uses generated demo data and the default keys; your desk may
provide different views, grouping presets, and shortcuts.

- [How to think about Geode](#how-to-think-about-geode)
- [A first walkthrough](#a-first-walkthrough)
- [Try the other tools](#try-the-other-tools)
- [Make it your workspace](#make-it-your-workspace)
- [When something looks unexpected](#when-something-looks-unexpected)
- [A small key reference](#a-small-key-reference)

## How to think about Geode

**Start with a question.** You might want to see where an exposure sits, compare
two ways of grouping a book, or inspect the market data for an underlying.
Geode presents numbers supplied by data sources and pricing services, and lets
you group, filter, and compare them. Understanding which data you are looking
at matters as much as reading the number. This is the practical meaning of its
philosophy: *a lens, not a brain*.

**Keep related information together.** Each panel is a *tile*. A blotter tile
shows a table with expandable groups; a timeseries tile shows a chart; a
market-data tile shows a document for an underlying. You can open several
tiles of the same kind. A *workspace* holds their arrangement, and a *stack*
holds several tiles in one position with one visible at a time.

**Share context, then make deliberate exceptions.** Geode calls its shared
viewing context the *frame*. It has three ideas you will use often:

| Idea | The question it answers | Example |
|---|---|---|
| Scope | Which data belongs in this view? | Only SPX, or only selected books |
| Grouping | How should the rows be organized? | Underlying, then book, then position |
| As-of | Which published data should I see? | The latest available data, or data available at an earlier time |

Tiles follow the parts of the frame relevant to them. A blotter can also keep
its own grouping, filter, or as-of setting. These exceptions appear in its
header. Workspaces organize tiles; switching workspace does not give you a
separate scope.

**Learn a few keys at a time.** Press `ctrl+k` to open the command palette,
type words describing an action, and press Enter to choose it. The palette is
the place to discover commands while you build up shortcuts. Mouse controls
are useful too; keyboard navigation becomes faster as the patterns settle in.

**Read the state alongside the value.** Live means the latest data Geode has
received, not a guarantee that the source is fresh. A stale marker, a failed
request, or an unsent edit changes what a displayed value means. Geode may keep
the last available result visible when an update fails; check its notices and
timestamps before treating it as current.

## A first walkthrough

The question for this walkthrough is: **where does SPX risk sit, and how does
the picture change when I organize it differently?** You will build two
blotters and open the related market data.

### 1. Start with the demo

If you are running from a checkout, start Geode from the repository directory:

```sh
cargo run -p geode-app -- --demo
```

This requires the build environment described in the [project README](../README.md#running-it).
If your desk provides a launch method, use that instead; the examples below
assume the demo's data and view names.

A fresh demo session opens an empty workspace. A previous session may restore
your tiles. Data loads asynchronously, so allow the first load to finish.
Demo mode still applies desk and personal configuration over its defaults;
your screen may therefore differ from this guide.

If you are returning to an existing demo session, use **Clear scope** and
**Return to live** in the palette before starting, so earlier shared filters
and time settings do not affect the exercise. The walkthrough adds new tiles.

In the instructions, `mod` means **Alt**, called **Option** on a Mac, unless
your configuration changes it. `ctrl` always means Control. A plus joins keys
held together (`mod+n`); spaces separate keys pressed in sequence (`g m`).
Uppercase `V` means Shift+V. For a command such as `:view tree`, press `:`,
type `view tree`, then press Enter.

### 2. Open and explore a blotter

Press `mod+n`, choose **Blotter**, and press Enter. You can also open the
palette with `ctrl+k` and search for **Add a tile**.

Run `:view tree` to select the demo's compact risk view. It includes columns
such as delta, gamma, vega, NPV, and trading P&L. Press `ctrl+3` to use the
demo grouping **underlying → book → position**.

Move down and up with `j` and `k`. On a group row, press Space to expand it;
press Space again to collapse it. Expand an underlying and then a book to
reach the individual positions. Use `h` and `l` to move between columns.

Try `:sort delta01 abs` to put the largest absolute delta exposures first
within their groups. Run `:sort clear` to return to the unsorted order. If
labels feel cramped, `:autosize` fits the columns to the loaded content it
has measured.

You have changed how the data is presented. Sorting and expanding groups do
not change which positions belong to the view.

### 3. Find an underlying, then narrow the scope

With the top-level underlying rows visible, press `/`, type `SPX`, and press
Enter. Use `n` to move to the next match. Find helps you navigate the blotter;
it does not narrow the data used to calculate its totals.

Now narrow the shared scope:

1. Press `mod+p` to open the dimension picker.
2. Type `underlying_ref` and press Enter to choose that dimension.
3. Wait for the values to load. If any values are already ticked, press
   `ctrl+x` to clear those ticks.
4. Type `SPX`, highlight the exact value, press Tab to tick it, then Enter
   to apply it.

The scope bar now shows the underlying selection. The blotter should contain
only the matching risk. Other tiles that follow this scope will use it too.

The picker supports several values: Tab toggles a tick; Enter applies the
selection. Highlighting a different row alone does not replace existing ticks.

Press `mod+z` to undo the scope change, then `mod+shift+z` to redo it. To remove
all shared scope constraints, use **Clear scope** in the palette. Keep SPX
selected for the next step.

### 4. Compare two views of the same scope

Press `mod+n` and add another Blotter. Run `:view tree` in it, then:

```text
:group book, underlying_ref, position_ref
```

This pins the new tile's grouping. It still follows the SPX scope, but its
rows are organized by book first. The first blotter still follows the shared
grouping.

Press `ctrl+1`. In the demo, that changes the shared grouping to
`lhu → underlying_ref → position_ref`. The first blotter changes; the pinned
one keeps its book-first arrangement. Look at the pinned tile's header to see
that exception.

Move between tiles with `mod+h/j/k/l` for left/down/up/right, or click the
tile you want. In the pinned tile, run `:unpin` to make its grouping follow the
frame again. Press `ctrl+3` to return both to underlying-first grouping.

This distinction repeats throughout Geode: **shared actions coordinate views;
tile commands let one view answer a different question.** A `:` command
affects the focused tile. It does not change the frame.

For example, `:filter underlying_ref = 'SPX'` adds an expression filter to one
blotter; `:filter clear` removes its local filters. A local expression still
has to satisfy the shared scope, so an SPX filter combined with an SX5E shared
scope would produce no matching rows.

### 5. Open the market data behind a row

In a blotter, place the cursor on the SPX underlying row or one of its
descendants. Press `g`, then `m`.

Geode offers tiles that can open on that underlying. Choose the CVI
market-data panel. It opens beside the blotter with SPX already selected.
The CVI panel displays volatility parameters; the dividend panel is another
available document view.

If you get the ordinary tile picker, the cursor did not identify a single
underlying. Return to an underlying-first blotter and place the cursor on SPX.

The new panel has taken the underlying from the row you opened it on. Moving
the blotter cursor afterward does not retarget that panel. You can keep it
beside your risk while investigating another row.

### 6. Make room without losing your arrangement

Focus a tile and press `mod+f` to make it fullscreen. Press it again to return
to the arrangement. The status bar tells you when fullscreen is hiding tiles.

To keep two tiles in one position, focus one and press
`mod+shift+h/j/k/l` toward its neighbor. This pulls the neighbor into a stack.
Use `mod+[` and `mod+]` to cycle through the stack. Press `mod+s` to split its
members back into separate tiles; they receive equal space.

Press `mod+2` to switch to workspace 2, then `mod+1` to return to workspace 1.
Your layouts stay in place. This lets you keep a risk arrangement on one
workspace and a pricing or charting arrangement on another.

### 7. Check the time context

Press `mod+t` to open the as-of selector. Choose a recent publication if one
is available, or use Custom to enter a time. Following tiles show data
available at that time. A fresh demo database may have little history, so an
earlier time can legitimately have no data.

Use **Return to live** in the palette when you finish.

A blotter can keep its own time context with `:asof <time>`. There is an
important difference between `:asof live`, which pins that tile to live, and
`:asof clear`, which makes it follow the shared as-of again. Returning the
frame to live does not remove a tile's historical pin.

## Try the other tools

### Chart a series

Add a **Timeseries** tile with `mod+n`. Run these commands one at a time:

```text
:add SPX.close@demo_kdb
:range 1m
:freq 1d
```

You should see a month of generated SPX closing data in daily buckets once
the fetch completes. Here `1m` is a month in the range command; `1m` in the
frequency command would mean one-minute buckets. The explicit `@demo_kdb`
identifies the demo source.

Press `.` to explore the tile's action menu. Series names and the chart's
range belong to this tile. A frame scope selecting SPX does not automatically
add an SPX series to a chart. Demo history is synthetic and uses fixed weekday
sessions, so it is useful for learning the controls rather than interpreting
market moves.

### Build a pricing sheet

Add a **Pricer** tile. Press `o` to open its entry bar and enter:

```text
SPX 3m 100% C
```

This describes one SPX call with a three-month tenor and a percentage strike.
Press Enter to add it, then Escape to leave the entry bar. The line displays
the pricing service's results and status. Result cells are read-only; use
`i` or Enter on an editable input cell to change its definition. Edits request
new prices. `u` undoes a sheet edit and `ctrl+r` redoes it.

The sheet's prices come from its configured pricing adapter. Demo results are
for exploring the workflow. The sheet is separate from the risk blotter;
adding a line does not add a position to the risk data. The frame's shared
scope still hides pricer lines that do not match it: the header counts them
as **N hidden**, hidden lines keep pricing, and `:unscoped` shows every line.
A line you add that the scope hides lands in the sheet with a footer saying
so.

Sheets save automatically after an idle second following a change. Give one a
recognizable name with `:name first-look`; later, `:e first-look` opens it.
Watch for a **sheet not saved** notice: an edit appearing in the grid does not
prove it reached storage. Allow pending saves to finish before quitting.

### Understand market-data edits

In a market-data panel, edits form a draft over a received document. Editing
a cell does not immediately publish it. Press `.` to inspect the available
actions and their current availability.

If a new document arrives while you have unsent edits, the default policy
holds your draft against its previous base. `:rebase` moves the edits onto the
new document, reporting edits it cannot carry over; `:revert` discards the
draft. Read the panel's notice before choosing either.

Publishing is a separate **Upload** action that requires a configured target
and confirmation. It replaces the whole document. **Sent** means transport
succeeded; **confirmed** means a matching document came back. These are
different stages, and an **echo differs** notice means the returned document
did not match the submitted one. Learn that workflow with your desk before
using it on live market data.

## Make it your workspace

Geode remembers workspace layouts and supported tile settings between runs.
It does not restore every transient detail: for example, a blotter's cursor,
selection, expansion, and sort order are not saved. Pricer sheets have their
own storage; market-data drafts are still drafts when restored.

Open **Open settings** or **Keyboard shortcuts** through the palette to adjust how you
work. Configuration comes from built-in defaults, desk defaults, and personal
overrides. Changes made through the dialogs write to your personal layer.
Your desk's shared defaults let everyone start with familiar views, while
personal settings let you adapt the workspace.

The palette also has **Edit views**, **Edit groupings**, and **Edit scopes**.
A view chooses the data and columns; a grouping organizes rows; a saved scope
captures a reusable selection. Learn those distinctions before creating lots
of near-identical views. If an arrangement or definition is useful to others,
ask your desk's configuration owner about adding it to the shared defaults.

## When something looks unexpected

| What you notice | What to check |
|---|---|
| A tile has no rows | Check the shared scope, local filters, and as-of time. An empty intersection or a time before the first publication can be valid. |
| Two blotters disagree | Compare their views and header markers for grouping pins, local filters, unscoped mode, and local as-of settings. |
| A blotter ignores a grouping change | Run `:unpin` to resume following the shared grouping. |
| A blotter ignores the shared scope | Check its unscoped marker. `:unscoped` toggles this mode; running it again restores scope following. |
| A pricer header shows **N hidden**, or a line you added disappeared | The shared scope hides those lines; they are still in the sheet and still pricing. `:unscoped` on the pricer shows every line. |
| A value is blank or marked `mixed` | Blank does not mean zero. Some measures cannot be attributed at that grouping depth; `mixed` means an ungrouped dimension has several contributing values. |
| A number remains visible after an error | It may be the last good result. Read the notice and timestamp. |
| Letter keys type instead of moving | An input is active. Finish or cancel that input before using navigation keys. |
| An action is hard to find | Search the palette by its purpose. While typing a `:` command, Tab completes arguments. |

For source or configuration problems, add a **Diagnostics** tile. Its Sources
section shows loading and health, Data shows stored publications, and Config
shows configuration errors and where settings came from. Use `[` and `]` to
switch sections. When reporting a problem, include the affected tile, its
scope and time context, and the exact notice.

## A small key reference

These are the defaults. Start with the palette, tile focus, and Space; add
the rest as you need them.

| Keys | Use |
|---|---|
| `ctrl+k` | Find an action in the palette |
| `mod+n` | Add a tile |
| `mod+h/j/k/l` | Focus the tile left/down/up/right |
| `mod+1` … `mod+9` | Switch workspace |
| `mod+f` | Toggle fullscreen for the focused main tile |
| `ctrl+w` | Close the focused tile |
| `ctrl+1` … `ctrl+9` | Choose a shared grouping slot |
| `ctrl+0` | Clear the shared grouping slot and use each view's default grouping |
| `mod+p` | Pick dimension values for the shared scope |
| `mod+/` | Edit shared scope text |
| `mod+z` / `mod+shift+z` | Undo / redo shared scope changes |
| `mod+t` | Choose the shared as-of time |
| `:` | Enter a command for the focused tile |
| `/` | Find within a tile that supports it |
| `j/k`, `h/l`, Space | In a blotter: move rows, move columns, expand/collapse |
| `V` / `v`, then motions, then `y` | In a blotter: select rows / a cell block and copy as tab-separated text |

Copied blotter numbers are raw, unscaled values, even when the display uses a
scale such as thousands. Selection totals avoid counting both a group and its
children; columns that cannot meaningfully be added show a marker instead of
a total.

For more background, read the [Geode philosophy](PHILOSOPHY.md). The
[documentation index](README.md) links to detailed behavior and configuration
references when you need them.
