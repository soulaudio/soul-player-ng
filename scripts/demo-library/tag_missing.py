import sys, re, pathlib, mutagen
from mutagen.id3 import ID3, TIT2, TPE1, TPE2, TALB, TRCK, TDRC, TCON
ART = "Soul Demo Ensemble"
YEARS = {"Direct Stream Digital": "2026", "Surround Fold-Down": "2026"}
root = pathlib.Path(sys.argv[1])
for f in sorted(root.rglob("*")):
    if not f.is_file() or f.name == "cover.jpg":
        continue
    album = f.parent.name
    m = re.match(r"(\d+) - (.+)\.[^.]+$", f.name)
    track, title = m.group(1).lstrip("0"), m.group(2)
    mf = mutagen.File(f, easy=False)
    has = False
    if mf is not None and mf.tags is not None:
        keys = {k.lower() for k in mf.tags.keys()}
        has = any(k in keys for k in ("title", "tit2", "\xa9nam"))
    if has:
        print("ok      ", album, "/", f.name); continue
    year = YEARS.get(album, "2026")
    if f.suffix in (".dsf", ".dff", ".wav", ".aiff"):
        mf = mutagen.File(f)
        if mf.tags is None:
            mf.add_tags()
        t = mf.tags
        for frame in (TIT2(encoding=3, text=title), TPE1(encoding=3, text=ART), TPE2(encoding=3, text=ART),
                      TALB(encoding=3, text=album), TRCK(encoding=3, text=track),
                      TDRC(encoding=3, text=year), TCON(encoding=3, text="Electronic")):
            t.add(frame)
        mf.save()
    else:
        mf = mutagen.File(f, easy=True)
        if mf.tags is None:
            mf.add_tags()
        mf["title"], mf["artist"], mf["album"] = title, ART, album
        mf["albumartist"], mf["tracknumber"], mf["date"], mf["genre"] = ART, track, year, "Electronic"
        mf.save()
    back = mutagen.File(f)
    print("tagged  ", album, "/", f.name, "->", type(back).__name__)
