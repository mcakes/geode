# Theme contrast proposal — individual palettes

Open `index.html` in a browser and use the theme selector. All 44 bundled named
variants have an explicit palette and a rationale in `palettes.json`. Light and
dark variants are considered separately. This supersedes the generic light/dark
palettes in the first mockup. The reviewed palette colors are now installed in `assets/themes`. The snapshot
in `before.json` preserves the original inputs for the comparisons.

Design decisions:

- Start from each variant's bundled accents, neutrals, background and foreground.
  Preserve characteristic saturation: Modus remains vivid, Alduin stays muted.
- Use different lightnesses and a neutral where that fits the theme. Do not
  force five equally vivid hues into every chart.
- Keep palettes that already fit and separate well: Bloomberg Modern,
  TradingView Dark, Modus Operandi and Modus Vivendi retain their hue choices.
- Nord uses its existing cyan, gold, mauve, deeper blue and sage. Everforest Dark
  uses teal, peach, moss, dusty rose and its cream foreground. Twilight uses
  candlelit gold, blue-grey, olive, terracotta and silver.
- Avoid changing unrelated theme colors or assigning a primary-series role
  where the user has not chosen one.

Review evidence:

- All 44 proposed palettes contain five distinct colors.
- The mockup measures every stroke against its theme background. Adjustment
  targets 3.2:1 for headroom above the existing 3:1 runtime requirement.
- `measurements.json` includes the smallest pairwise Euclidean OKLab distance.
  This is a screening measurement; 0.07 is a local review floor, not an
  accessibility standard or proof of distinguishability in every condition.
- The runtime tests sweep contrast and separation after GPUI theme resolution.
  These mockups are illustrative, with no color-vision-deficiency simulation.
  Keep series numbers and labels; mathematical checks do not prove visual
  distinguishability for every user.

Seven exported examples also show the previous chip proposal: require 4.5:1
text contrast, adjust failing text to 4.7:1 for headroom, preserve readable pairs.
They show Default Light/Dark, Nord, Everforest Dark, Ayu Light, Bloomberg and
Twilight. Other gallery entries focus on chart colors only.

“Before” reproduces the original snapshotted theme values, pinned parser fallback and Geode's
current OKLCH readability adjustment. Default's `chart_1` through `chart_5`
entries are ignored by GPUI Component 0.6.2, whose parser expects `chart.1`
through `chart.5`; the mockup correctly uses the resulting blue fallback ramp.
Synthetic data, stroke widths, layout and surfaces match on both sides.

The generator uses Python double precision and exports 8-bit CSS colors; small
rounding differences from GPUI's f32 rendering are possible. This is a review
aid, not the final runtime regression test.

To regenerate on macOS, using the locally cached GPUI Component 0.6.2 dictionary:

```sh
python3 docs/theme-review/render_svg.py
swift -module-cache-path /private/tmp/geode-swift-module-cache \
  docs/theme-review/render.swift docs/theme-review/*.svg
```

This generates the HTML gallery, measurements, seven SVG comparisons, and PNG
previews rendered from SVG with AppKit. No network access is required.
