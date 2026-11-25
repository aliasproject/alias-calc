# Fonts

Subsets of [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) v2.304, the aliasOS monospace font, under the SIL Open Font License 1.1 (`OFL.txt`). They're compiled into the binary, so nothing is looked up through fontconfig at startup.

Each keeps printable ASCII plus × ÷ − ± ⌫ …, with no hinting or layout tables (the renderer doesn't use them). About 9 KB each. To rebuild them from the release's `fonts/ttf/` files:

```sh
for w in Regular Light; do
  uvx --from fonttools pyftsubset JetBrainsMono-$w.ttf \
    --unicodes="U+0020-007E,U+00D7,U+00F7,U+00B1,U+2212,U+232B,U+2026" \
    --layout-features='' --no-hinting --desubroutinize \
    --name-IDs='0,1,2,3,4,5,6,13,14' \
    --drop-tables+=GSUB,GPOS,GDEF,DSIG,STAT,gasp,prep,fpgm,cvt \
    --output-file=JetBrainsMono-$w-subset.ttf
done
```
