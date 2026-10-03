# Bundled themes

Each named JSON variant is a complete theme selected through Settings, the
command palette, or `[theme] name` in `app.toml`. `geode-shell::theme` embeds
the files; a new file must also be registered in its `BUNDLED` list.

## Adapted palettes

| Family | Selectable names | Upstream palette |
| --- | --- | --- |
| Kanagawa | Kanagawa Wave, Kanagawa Dragon, Kanagawa Lotus | [rebelot/kanagawa.nvim](https://github.com/rebelot/kanagawa.nvim/tree/bb85e4bfc8d89b0e62c8fa53ccdd13d12e2f77b3), `lua/kanagawa/colors.lua` and `themes.lua` |
| Rosé Pine | Rosé Pine, Rosé Pine Moon, Rosé Pine Dawn | [rose-pine/neovim](https://github.com/rose-pine/neovim/tree/ff483051a47e27d84bdef47703538df1ed9f4a47), `lua/rose-pine/palette.lua` |
| GitHub | GitHub Light, GitHub Dark Dimmed | [primer/github-vscode-theme](https://github.com/primer/github-vscode-theme/tree/cd78e5e4e7bcf132a6f428ae0f32264bb1b729cf), `src/colors.js`, with `@primer/primitives` 7.10.0 `light` and `dark_dimmed` palettes |

The upstream MIT notices are preserved in [licenses](licenses). These are
Geode adaptations: surface and foreground colors retain the palette's identity,
while list selection, control states, and semantic colors map to GPUI Component
0.6.2's token names. No runtime downloads or new dependencies are required.
Lotus, Dawn, and GitHub Light are light variants; the others are dark.

Muted text is adjusted to clear 4.5:1 on the base, raised, and popover surfaces.
Gain/loss colors use separate red and green/teal roles; Rosé Pine uses its
upstream leaf color for gains. The five chart series are chosen individually:

- Wave: blue, gold, rose, green, and warm white.
- Dragon: blue-grey, sand, red, green, and cool white.
- Lotus: blue, orange, rose, green, and ink.
- Rosé Pine and Moon: pine, gold, love, iris, and pale text.
- Dawn: pine, darkened gold, love, iris, and ink.
- GitHub: blue, orange, green, purple, and pink.

Chart strokes target at least 3.2:1 against the base surface before runtime
resolution. The existing runtime sweep requires at least 3:1 and a pairwise
OKLab distance of 0.07 after resolution. This is a palette screening measure,
not proof of distinguishability for every viewer; retain series labels and
numeric signs. Chip and control painters separately enforce text contrast
against their actual fills.

Color keys must follow the pinned parser (`chart.1` through `chart.5`,
`chart.bullish`, and `chart.bearish`). Unknown keys are silently ignored upstream
and can substitute fallback colors. Theme parse failures are reported as
warnings and omit the affected family; an unknown configured name falls back
to Default Dark.
