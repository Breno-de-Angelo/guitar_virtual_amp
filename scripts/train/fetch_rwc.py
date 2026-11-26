"""Downloads part of the RWC Music Database audio (RWC 2.0, CC BY-NC 4.0,
https://zenodo.org/records/17177919) without fetching the whole 13.4 GB
archive: the zip's directory is read with HTTP range requests and only the
members of the chosen collections are copied out. The aligned MIDI comes
from https://github.com/rwc-music/rwc-annotations (CC BY-NC 4.0).

Usage: python fetch_rwc.py --out ~/.cache/gva_test/rwc [--collections P,J,R]
writes <out>/audio/RWC_P001.wav ... and <out>/midi/RWC_P001.mid ...
Cite Goto et al. (ISMIR 2002) and Balke et al. (TISMIR 2026) when using it.
"""
import argparse
import io
import shutil
import urllib.request
import zipfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ZIP_URL = "https://zenodo.org/records/17177919/files/RWC-CGJPR.zip?download=1"
MIDI_URL = "https://raw.githubusercontent.com/rwc-music/rwc-annotations/main/01_annotations_preprocessed/MIDI_aligned/RWC-{c}/{id}.mid"
BEATS_URL = "https://raw.githubusercontent.com/rwc-music/rwc-annotations/main/01_annotations_preprocessed/beats/RWC-{c}/{id}.csv"


class HttpFile(io.RawIOBase):
    """Seekable read-only file over HTTP range requests."""

    def __init__(self, url):
        r = urllib.request.urlopen(urllib.request.Request(url, method="HEAD"))
        self.url, self.size, self.pos = r.geturl(), int(r.headers["Content-Length"]), 0

    def seekable(self):
        return True

    def readable(self):
        return True

    def tell(self):
        return self.pos

    def seek(self, offset, whence=0):
        self.pos = offset if whence == 0 else self.pos + offset if whence == 1 else self.size + offset
        return self.pos

    def readinto(self, buf):
        if not len(buf) or self.pos >= self.size:
            return 0
        end = min(self.size, self.pos + len(buf)) - 1
        req = urllib.request.Request(self.url, headers={"Range": f"bytes={self.pos}-{end}"})
        data = urllib.request.urlopen(req, timeout=120).read()
        buf[: len(data)] = data
        self.pos += len(data)
        return len(data)


def open_zip():
    return zipfile.ZipFile(io.BufferedReader(HttpFile(ZIP_URL), buffer_size=16 << 20))


def extract(name, dest):
    if dest.exists():
        return f"{dest.name}: present"
    tmp = dest.with_suffix(".part")
    with open_zip() as z, z.open(name) as src, open(tmp, "wb") as out:
        shutil.copyfileobj(src, out, 16 << 20)
    tmp.rename(dest)
    return f"{dest.name}: {dest.stat().st_size / 1e6:.0f} MB"


def fetch(url, dest):
    if not dest.exists():
        try:
            urllib.request.urlretrieve(url, dest)
        except Exception as err:  # beats are missing for some pieces
            return f"{dest.name}: {err}"
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--collections", default="P")
    ap.add_argument("--jobs", type=int, default=6)
    args = ap.parse_args()
    out = args.out.expanduser()
    (out / "audio").mkdir(parents=True, exist_ok=True)
    (out / "midi").mkdir(exist_ok=True)
    (out / "beats").mkdir(exist_ok=True)
    colls = args.collections.split(",")
    with open_zip() as z:
        members = [i.filename for i in z.infolist() if i.filename.endswith(".wav") and i.filename.split("/")[-2] in {f"RWC-{c}" for c in colls}]
    print(f"{len(members)} audio files", flush=True)
    ids = [Path(m).stem for m in members]
    for rid in ids:
        c = rid.split("_")[1][0]
        for url, d in ((MIDI_URL, "midi"), (BEATS_URL, "beats")):
            msg = fetch(url.format(c=c, id=rid), out / d / (rid + (".mid" if d == "midi" else ".csv")))
            if msg:
                print(msg)
    with ThreadPoolExecutor(args.jobs) as ex:
        for msg in ex.map(lambda m: extract(m, out / "audio" / Path(m).name), members):
            print(msg, flush=True)


if __name__ == "__main__":
    main()
