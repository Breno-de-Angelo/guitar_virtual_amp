"""ONNX export of a fingering checkpoint, checked against PyTorch with
onnxruntime; writes `<onnx>.json` with the op list, the max logit difference
and the decoding costs (from `meta.json` next to the checkpoint).

Inputs `frets` [1, T, 6] int64 and `cont` [1, T, 16] float32 (T dynamic),
plus `patch` [1, T, 150, 28] float32 for an audio model (`audio.py`'s uint8
patch values as floats); output `logits` [1, T, 6].

Usage: python export.py runs/ship_s1/model.pt runs/ship_s1/model.onnx [--data data.json]
"""
import argparse
import json
from collections import Counter
from pathlib import Path

import numpy as np
import torch

from common import features, sort_notes
from model import Tagger


def export(ckpt, onnx_path, recs, audio_dir=None, opset=17):
    """`recs` need `tones` (train.attach_audio) for an audio model."""
    import onnx
    import onnxruntime as ort

    ck = torch.load(ckpt, map_location="cpu")
    model = Tagger(**ck["cfg"])
    model.load_state_dict(ck["state"])
    model.eval()
    audio = model.audio is not None
    recs = [dict(r, notes=[r["notes"][i] for i in sort_notes(r["notes"])]) for r in recs]
    if audio and audio_dir is not None and "tones" not in recs[0]:
        from train import attach_audio

        recs = attach_audio(recs, Path(audio_dir))

    def inputs(r, n):
        notes = r["notes"][:n]
        f, c, _ = features(notes, r["tuning"])
        feeds = {"frets": f[None], "cont": c[None]}
        if audio:
            from train import note_patches

            feeds["patch"] = note_patches(notes, r["tones"][0])[None].astype(np.float32)
        return feeds

    first = inputs(recs[0], 100)
    names = list(first)
    dyn = {k: {1: "notes"} for k in names + ["logits"]}
    torch.onnx.export(model, tuple(torch.from_numpy(first[k]) for k in names), str(onnx_path), input_names=names,
                      output_names=["logits"], dynamic_axes=dyn, opset_version=opset, dynamo=False)
    m = onnx.load(str(onnx_path))
    ops = Counter(n.op_type for n in m.graph.node)
    sess = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    diff, same = 0.0, 0
    total = 0
    for r in recs:
        for n in (1, 7, 128, min(len(r["notes"]), 300)):
            feeds = inputs(r, n)
            if feeds["frets"].shape[1] == 0:
                continue
            with torch.no_grad():
                ref = model(*(torch.from_numpy(feeds[k]) for k in names)).numpy()
            got = sess.run(None, feeds)[0]
            ok = feeds["frets"] != 25
            diff = max(diff, float(np.abs(ref - got)[ok].max()))
            same += int((ref.argmax(-1) == got.argmax(-1)).sum())
            total += ref.shape[1]
    meta_path = Path(ckpt).with_name("meta.json")
    meta = json.load(open(meta_path)) if meta_path.exists() else {}
    info = {"ops": dict(sorted(ops.items())), "opset": opset, "max_abs_logit_diff": diff, "argmax_agreement": same / max(1, total),
            "params": sum(p.numel() for p in model.parameters()), "decode": meta.get("decode"), "cfg": ck["cfg"],
            "inputs": {"frets": "[1, T, 6] int64, fret on strings 1..6 (high e first), 25 = unplayable",
                       "cont": "[1, T, 16] float32, common.features"}}
    if audio:
        info["inputs"]["patch"] = "[1, T, 150, 28] float32, audio.patches (uint8 values as floats)"
    json.dump(info, open(str(onnx_path) + ".json", "w"), indent=1)
    print(f"exported {onnx_path}: max |diff| {diff:.2e}, argmax agreement {info['argmax_agreement']:.4f}, ops {dict(ops)}", flush=True)
    return info


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("ckpt", type=Path)
    ap.add_argument("onnx", type=Path)
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--audio", type=Path, default=None, help="audio.py output, for an audio model")
    args = ap.parse_args()
    recs = [r for r in json.load(open(args.data))["recordings"] if r["split"] == "val" and r["source"] != "goat"][:8]
    export(args.ckpt, args.onnx, recs, args.audio)


if __name__ == "__main__":
    main()
