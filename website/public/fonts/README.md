# Fonts

**IBM Plex Sans** (400, 600) and **IBM Plex Mono** (400), subset to Latin plus
the punctuation this page sets, and converted to woff2. 44 KB for all three.

Self-hosted rather than loaded from a font CDN: one less third party on a page
whose whole pitch is that the app does not phone home, and it keeps the
Content-Security-Policy at `font-src 'self'`.

Licensed under the [SIL Open Font License 1.1](https://github.com/IBM/plex/blob/master/LICENSE.txt),
which permits redistribution of the originals and of modified copies (a subset
is a modification) as long as they are not sold on their own and the license
travels with them. Copyright 2017 IBM Corp.

Regenerate from the upstream TTFs with `pyftsubset`:

```sh
pyftsubset IBMPlexSans-Regular.ttf \
  --unicodes='U+0020-007E,U+00A0-00FF,U+2013-2014,U+2018-201A,U+201C-201E,U+2026,U+00B7,U+00A9,U+2122' \
  --layout-features='kern,liga,calt,tnum' \
  --flavor=woff2 --output-file=plex-sans-400.woff2
```
