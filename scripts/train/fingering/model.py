"""Fingering tagger: per-note string logits from note features (`common.features`).

Inputs: `frets` [B, T, 6] int64 (fret of the note on each string, 25 =
unplayable there) and `cont` [B, T, 16] float32; with `audio` > 0 also
`patch` [B, T, 150, 28] float32, the note's pitch-relative spectrogram patch
as stored by `audio.py` (uint8 values 0..255 as floats). Output: `logits`
[B, T, 6], strings 1..6 (high e first), unplayable strings at -1e4.

Two encoders, both built from ops tract handles (Gather, MatMul/Gemm, Conv,
Softmax, ReduceMean, Relu, LSTM): a pre-norm Transformer with a depthwise
convolution for order (no position tables, so any length works) and a BiLSTM.
"""
import torch
from torch import nn

from common import INVALID, N_CONT

PATCH_F, PATCH_T = 150, 28  # audio.PATCH_F, audio.PATCH_T
Q_SCALE = 15.0  # audio.Q_SCALE


class LayerNorm(nn.Module):
    """LayerNorm written out (ReduceMean/Sub/Mul/Sqrt), for older ONNX importers."""

    def __init__(self, d, eps=1e-5):
        super().__init__()
        self.weight = nn.Parameter(torch.ones(d))
        self.bias = nn.Parameter(torch.zeros(d))
        self.eps = eps

    def forward(self, x):
        m = x.mean(-1, keepdim=True)
        v = ((x - m) ** 2).mean(-1, keepdim=True)
        return (x - m) / torch.sqrt(v + self.eps) * self.weight + self.bias


class Block(nn.Module):
    def __init__(self, d, heads, ff, dropout):
        super().__init__()
        self.h = heads
        self.n1, self.n2, self.n3 = LayerNorm(d), LayerNorm(d), LayerNorm(d)
        self.qkv = nn.Linear(d, 3 * d)
        self.proj = nn.Linear(d, d)
        self.conv = nn.Conv1d(d, d, 5, padding=2, groups=d)
        self.ff = nn.Sequential(nn.Linear(d, ff), nn.ReLU(), nn.Dropout(dropout), nn.Linear(ff, d))
        self.drop = nn.Dropout(dropout)

    def forward(self, x, bias):
        # Local order: depthwise conv over time.
        x = x + self.drop(self.conv(self.n3(x).transpose(1, 2)).transpose(1, 2))
        b, t, d = x.shape
        q, k, v = self.qkv(self.n1(x)).reshape(b, t, 3, self.h, d // self.h).permute(2, 0, 3, 1, 4)
        att = torch.matmul(q, k.transpose(-1, -2)) * (d // self.h) ** -0.5
        if bias is not None:
            att = att + bias
        y = torch.matmul(torch.softmax(att, -1), v).transpose(1, 2).reshape(b, t, d)
        x = x + self.drop(self.proj(y))
        return x + self.drop(self.ff(self.n2(x)))


class AudioEnc(nn.Module):
    """Per-note embedding of a pitch-relative patch [N, 150, 28]: log
    magnitude relative to the patch's maximum, three small convolutions,
    then a linear layer over the remaining frequency positions (harmonic n
    sits at a fixed bin, so position matters)."""

    def __init__(self, out=64, ch=16):
        super().__init__()
        self.c1 = nn.Conv2d(1, ch, (5, 3), padding=(2, 1))
        self.c2 = nn.Conv2d(ch, 2 * ch, 3, padding=1)
        self.c3 = nn.Conv2d(2 * ch, 2 * ch, 3, padding=1)
        self.fc = nn.Linear(2 * ch * (PATCH_F // 6), out)

    def forward(self, q):
        x = q / Q_SCALE
        x = x - x.amax(dim=(1, 2), keepdim=True)
        x = torch.clamp(x, min=-8.0) * 0.25 + 1.0
        x = x.unsqueeze(1)
        x = nn.functional.max_pool2d(torch.relu(self.c1(x)), (1, 2))  # 150 x 14
        x = nn.functional.max_pool2d(torch.relu(self.c2(x)), (2, 2))  # 75 x 7
        x = nn.functional.max_pool2d(torch.relu(self.c3(x)), (3, 7))  # 25 x 1
        return torch.relu(self.fc(x.flatten(1)))


class Tagger(nn.Module):
    def __init__(self, kind="transformer", d=128, layers=4, heads=4, ff=256, dropout=0.1, emb=16, audio=0, audio_ch=16):
        super().__init__()
        self.kind = kind
        self.fret_emb = nn.Embedding(6 * (INVALID + 1), emb)
        self.register_buffer("offsets", torch.arange(6) * (INVALID + 1), persistent=False)
        self.audio = AudioEnc(audio, audio_ch) if audio > 0 else None
        self.inp = nn.Sequential(nn.Linear(6 * emb + N_CONT + audio, d), nn.ReLU(), nn.Linear(d, d))
        if kind == "transformer":
            self.blocks = nn.ModuleList(Block(d, heads, ff, dropout) for _ in range(layers))
            self.norm = LayerNorm(d)
        else:
            self.lstm = nn.LSTM(d, d // 2, num_layers=layers, batch_first=True, bidirectional=True, dropout=dropout)
        self.head = nn.Linear(d, 6)

    def forward(self, frets, cont, patch=None, pad=None):
        """`pad` [B, T] bool, True on padding (training only)."""
        b, t, _ = frets.shape
        e = self.fret_emb(frets + self.offsets).reshape(b, t, -1)
        parts = [e, cont]
        if self.audio is not None:
            parts.append(self.audio(patch.reshape(b * t, PATCH_F, PATCH_T)).reshape(b, t, -1))
        x = self.inp(torch.cat(parts, -1))
        if self.kind == "transformer":
            bias = None
            if pad is not None:
                bias = pad[:, None, None, :].float() * -1e4
                keep = (~pad).float()[..., None]
            for blk in self.blocks:
                if pad is not None:
                    x = x * keep  # padding stays out of the convolution
                x = blk(x, bias)
            x = self.norm(x)
        elif pad is not None:
            lengths = (~pad).sum(1).cpu()
            packed = nn.utils.rnn.pack_padded_sequence(x, lengths, batch_first=True, enforce_sorted=False)
            x, _ = nn.utils.rnn.pad_packed_sequence(self.lstm(packed)[0], batch_first=True, total_length=t)
        else:
            x, _ = self.lstm(x)
        logits = self.head(x)
        valid = (frets != INVALID).float()
        return logits * valid + (valid - 1.0) * 1e4
