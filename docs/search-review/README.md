# Search continuity studies

Open `index.html` locally. This is a browser mockup, not a change to Geode's search behavior. It uses the bundled Inter / JetBrains Mono fonts and semantic colors sampled from Default Light / Dark. Numeric data is illustrative. The small matching function illustrates ranking and character highlighting; it is not the production scorer.

Compare:

- **A — Ranked rows:** individual matches sorted globally, with the existing line-number rule and source indentation. No chevrons because there is no contiguous branch to expand.
- **B — Ranked tree (recommended):** matches plus their ancestors; each branch takes the relevance of its best match and sibling branches reorder by that relevance. Parents appear once. Chevrons collapse and expand the filtered branch.
- **C — Original tree:** full tree in source order, with matches emphasized and other rows muted. Chevrons work normally. Strong continuity, weaker result compression and no global relevance ordering.

Controls: shared query, On / Relative / Off line numbers, Light / Dark theme, empty-query and reset buttons. Click a row, then use Up / Down (or j / k); Enter demonstrates returning to the original tree at that row. Escape demonstrates cancellation or restarts the mockup. Left / Right fold the selected group in B and C. The restored mock tree uses its initial fully expanded state; it does not simulate live data or an existing application session.

All numbers address displayed rows, including context rows, matching the current app's numbering convention. Keyboard selection visits those same displayed rows. The original aggregate values stay visible on parents; they do not become aggregates of filtered matches.

B was selected for production: hierarchy-aware ordering runs on the search worker, and the native table retains its own cell and header formatting. The app uses Tab or a chevron click to fold search branches; Left / Right remain available for editing the query. No performance claims are made by this tiny prototype.

For a local browser smoke check, open `index.html?test=1`; success sets `document.body.dataset.test` to `passed`.
