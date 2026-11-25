# Fonts

| Face | Role | Subset to |
|---|---|---|
| Fraunces 72pt Soft SemiBold | the wordmark, and nothing else | ASCII |
| Work Sans 400 / 600 | everything the page reads as text | Latin-1 + punctuation |
| JetBrains Mono 400 | the install command | Latin-1 + punctuation |

80 KB for all four, because each is subset to the characters this page actually
sets. Fraunces only ever renders `Soul Player`, so it gets plain ASCII.

Self-hosted rather than loaded from a font CDN: one less third party on a page
whose whole pitch is that the app does not phone home, and it keeps the
Content-Security-Policy at `font-src 'self'`.

All three families are under the [SIL Open Font License 1.1](https://openfontlicense.org),
which permits redistributing the originals and modified copies (a subset is a
modification) as long as they are not sold on their own and the license travels
with them. Copyright: Fraunces by Undercase Type, Work Sans by Wei Huang,
JetBrains Mono by JetBrains s.r.o.

Regenerate from the upstream TTFs with `pyftsubset`:

```sh
TEXT='U+0020-007E,U+00A0-00FF,U+2013-2014,U+2018-201A,U+201C-201E,U+2026,U+00B7,U+00A9,U+2122'

pyftsubset Fraunces72ptSoft-SemiBold.ttf --unicodes='U+0020-007E' \
  --layout-features='kern,liga,calt,tnum' --flavor=woff2 --output-file=fraunces-600.woff2
pyftsubset WorkSans-Regular.ttf --unicodes="$TEXT" \
  --layout-features='kern,liga,calt,tnum' --flavor=woff2 --output-file=work-sans-400.woff2
pyftsubset WorkSans-SemiBold.ttf --unicodes="$TEXT" \
  --layout-features='kern,liga,calt,tnum' --flavor=woff2 --output-file=work-sans-600.woff2
pyftsubset JetBrainsMono-Regular.ttf --unicodes="$TEXT" \
  --layout-features='kern,liga,calt,tnum' --flavor=woff2 --output-file=jetbrains-mono-400.woff2
```
