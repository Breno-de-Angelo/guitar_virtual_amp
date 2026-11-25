"""ONNX export of a fingering checkpoint, checked against PyTorch with
onnxruntime; writes `<onnx>.json` with the op list, the max logit difference
and the decoding costs (from `meta.json` next to the checkpoint).

Inputs `frets` [1, T, 6] int64 and `cont` [1, T, 16] float32 (T dynamic),
output `logits` [1, T, 6].

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


def export(ckpt, onnx_path, recs, opset=17):
    import onnx
    import onnxruntime as ort

    ck = torch.load(ckpt, map_location="cpu")
    model = Tagger(**ck["cfg"])
    model.load_state_dict(ck["state"])
    model.eval()
    recs = [dict(r, notes=[r["notes"][i] for i in sort_notes(r["notes"])]) for r in recs]
    f, c, _ = features(recs[0]["notes"][:100], recs[0]["tuning"])
    args = (torch.from_numpy(f[None]), torch.from_numpy(c[None]))
    torch.onnx.export(model, args, str(onnx_path), input_names=["frets", "cont"], output_names=["logits"],
                      dynamic_axes={"frets": {1: "notes"}, "cont": {1: "notes"}, "logits": {1: "notes"}},
                      opset_version=opset, dynamo=False)
    m = onnx.load(str(onnx_path))
    ops = Counter(n.op_type for n in m.graph.node)
    sess = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    diff, same = 0.0, 0
    total = 0
    for r in recs:
        for n in (1, 7, 128, min(len(r["notes"]), 300)):
            f, c, _ = features(r["notes"][:n], r["tuning"])
            if len(f) == 0:
                continue
            with torch.no_grad():
                ref = model(torch.from_numpy(f[None]), torch.from_numpy(c[None])).numpy()
            got = sess.run(None, {"frets": f[None], "cont": c[None]})[0]
            ok = f[None] != 25
            diff = max(diff, float(np.abs(ref - got)[ok].max()))
            same += int((ref.argmax(-1) == got.argmax(-1)).sum())
            total += ref.shape[1]
    meta_path = Path(ckpt).with_name("meta.json")
    meta = json.load(open(meta_path)) if meta_path.exists() else {}
    info = {"ops": dict(sorted(ops.items())), "opset": opset, "max_abs_logit_diff": diff, "argmax_agreement": same / max(1, total),
            "params": sum(p.numel() for p in model.parameters()), "decode": meta.get("decode"), "cfg": ck["cfg"],
            "inputs": {"frets": "[1, T, 6] int64, fret on strings 1..6 (high e first), 25 = unplayable",
                       "cont": "[1, T, 16] float32, common.features"}}
    json.dump(info, open(str(onnx_path) + ".json", "w"), indent=1)
    print(f"exported {onnx_path}: max |diff| {diff:.2e}, argmax agreement {info['argmax_agreement']:.4f}, ops {dict(ops)}", flush=True)
    return info


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("ckpt", type=Path)
    ap.add_argument("onnx", type=Path)
    ap.add_argument("--data", type=Path, required=True)
    args = ap.parse_args()
    recs = [r for r in json.load(open(args.data))["recordings"] if r["split"] == "val"][:8]
    export(args.ckpt, args.onnx, recs)


if __name__ == "__main__":
    main()
