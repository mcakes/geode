# Vendored fonts

Both families are licensed under the SIL Open Font License 1.1 (`OFL.txt`
alongside each family's files), which permits vendoring/redistribution.
Registered at startup by `geode_shell::fonts::register` (bytes embedded via
`include_bytes!`, no runtime file I/O) — see that module for the
Inter-default / JetBrains-Mono-data-face mapping.

Only the static TTF weights actually used are vendored, not full family
archives.

## Inter — UI face

- Upstream: <https://github.com/rsms/inter>
- Version: v4.1 (release tag `v4.1`)
- Source: `Inter-4.1.zip` → `extras/ttf/` (static, non-"Display" cut) from
  <https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip>
- Files: `Inter-Regular.ttf`, `Inter-Medium.ttf`, `Inter-SemiBold.ttf`
- License: `inter/OFL.txt` (upstream `LICENSE.txt`, SIL OFL 1.1 text)

## JetBrains Mono — data face

- Upstream: <https://github.com/JetBrains/JetBrainsMono>
- Version: v2.304 (release tag `v2.304`)
- Source: `JetBrainsMono-2.304.zip` → `fonts/ttf/` from
  <https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip>
- Files: `JetBrainsMono-Regular.ttf`, `JetBrainsMono-Bold.ttf`
- License: `jetbrains-mono/OFL.txt` (upstream `OFL.txt`, SIL OFL 1.1 text)
