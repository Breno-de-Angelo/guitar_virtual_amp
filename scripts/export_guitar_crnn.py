"""Exports the guitar transcription CRNN to ONNX for guitar_core::transcribe::guitar_crnn.

The network is the "Regress_onset_offset_frame_velocity_CRNN" of Kong et al.,
"High-resolution Piano Transcription with Pedals by Regressing Onset and
Offset Times" (2021), here with the guitar weights of Riley et al., "GAPS: A
Large and Diverse Classical Guitar Dataset and Benchmark Transcription
Model" (ISMIR 2024), published under the MIT licence as `guitar-gaps.pth` in
https://huggingface.co/xavriley/midi-transcription-models.

This file re-declares the architecture with the parameter names of the
checkpoint (the STFT and mel filter weights are stored in it too), so only
PyTorch and onnx are needed. The ONNX graph takes 10 s of 16 kHz mono audio,
`audio: [1, 160000]`, and returns `onset, offset, frame, velocity`, each
`[1, 1001, 88]` (100 frames per second, MIDI 21-108).

Usage: python export_guitar_crnn.py guitar-gaps.pth out.onnx
"""

import sys

import torch
import torch.nn as nn
import torch.nn.functional as F

SAMPLE_RATE = 16000
SEGMENT = 10 * SAMPLE_RATE
N_FFT = 2048
HOP = SAMPLE_RATE // 100
MEL_BINS = 229
PITCHES = 88


class STFT(nn.Module):
    def __init__(self):
        super().__init__()
        bins = N_FFT // 2 + 1
        self.conv_real = nn.Conv1d(1, bins, N_FFT, stride=HOP, bias=False)
        self.conv_imag = nn.Conv1d(1, bins, N_FFT, stride=HOP, bias=False)

    def forward(self, x):
        x = F.pad(x[:, None, :], (N_FFT // 2, N_FFT // 2), mode="reflect")
        real = self.conv_real(x)[:, None].transpose(2, 3)
        imag = self.conv_imag(x)[:, None].transpose(2, 3)
        return real, imag


class Spectrogram(nn.Module):
    def __init__(self):
        super().__init__()
        self.stft = STFT()

    def forward(self, x):
        real, imag = self.stft(x)
        return real**2 + imag**2


class LogmelFilterBank(nn.Module):
    def __init__(self):
        super().__init__()
        self.melW = nn.Parameter(torch.zeros(N_FFT // 2 + 1, MEL_BINS))

    def forward(self, x):
        return 10.0 * torch.log10(torch.clamp(torch.matmul(x, self.melW), min=1e-10))


class ConvBlock(nn.Module):
    def __init__(self, cin, cout, momentum):
        super().__init__()
        self.conv1 = nn.Conv2d(cin, cout, (3, 3), padding=(1, 1), bias=False)
        self.conv2 = nn.Conv2d(cout, cout, (3, 3), padding=(1, 1), bias=False)
        # The reference passes `momentum` positionally, i.e. as `eps`.
        self.bn1 = nn.BatchNorm2d(cout, eps=momentum)
        self.bn2 = nn.BatchNorm2d(cout, eps=momentum)

    def forward(self, x):
        x = F.relu(self.bn1(self.conv1(x)))
        x = F.relu(self.bn2(self.conv2(x)))
        return F.avg_pool2d(x, kernel_size=(1, 2))


class AcousticModel(nn.Module):
    def __init__(self, momentum=0.01):
        super().__init__()
        self.conv_block1 = ConvBlock(1, 48, momentum)
        self.conv_block2 = ConvBlock(48, 64, momentum)
        self.conv_block3 = ConvBlock(64, 96, momentum)
        self.conv_block4 = ConvBlock(96, 128, momentum)
        self.fc5 = nn.Linear(1792, 768, bias=False)
        self.bn5 = nn.BatchNorm1d(768, momentum=momentum)
        self.gru = nn.GRU(768, 256, num_layers=2, batch_first=True, bidirectional=True)
        self.fc = nn.Linear(512, PITCHES)

    def forward(self, x):
        for block in (self.conv_block1, self.conv_block2, self.conv_block3, self.conv_block4):
            x = block(x)
        x = x.transpose(1, 2).flatten(2)
        x = F.relu(self.bn5(self.fc5(x).transpose(1, 2)).transpose(1, 2))
        x, _ = self.gru(x)
        return torch.sigmoid(self.fc(x))


class GuitarCRNN(nn.Module):
    def __init__(self, momentum=0.01):
        super().__init__()
        self.spectrogram_extractor = Spectrogram()
        self.logmel_extractor = LogmelFilterBank()
        self.bn0 = nn.BatchNorm2d(MEL_BINS, eps=momentum)
        self.frame_model = AcousticModel()
        self.reg_onset_model = AcousticModel()
        self.reg_offset_model = AcousticModel()
        self.velocity_model = AcousticModel()
        self.reg_onset_gru = nn.GRU(PITCHES * 2, 256, batch_first=True, bidirectional=True)
        self.reg_onset_fc = nn.Linear(512, PITCHES)
        self.frame_gru = nn.GRU(PITCHES * 3, 256, batch_first=True, bidirectional=True)
        self.frame_fc = nn.Linear(512, PITCHES)

    def forward(self, audio):
        x = self.logmel_extractor(self.spectrogram_extractor(audio))
        x = self.bn0(x.transpose(1, 3)).transpose(1, 3)
        frame = self.frame_model(x)
        onset = self.reg_onset_model(x)
        offset = self.reg_offset_model(x)
        velocity = self.velocity_model(x)
        x, _ = self.reg_onset_gru(torch.cat((onset, (onset**0.5) * velocity), dim=2))
        onset = torch.sigmoid(self.reg_onset_fc(x))
        x, _ = self.frame_gru(torch.cat((frame, onset, offset), dim=2))
        frame = torch.sigmoid(self.frame_fc(x))
        return onset, offset, frame, velocity


def main():
    checkpoint, out = sys.argv[1], sys.argv[2]
    model = GuitarCRNN()
    state = torch.load(checkpoint, map_location="cpu", weights_only=False)["model"]
    state = {k: v for k, v in state.items() if not k.endswith("num_batches_tracked")}
    model.load_state_dict(state, strict=True)
    model.eval()
    with torch.no_grad():
        torch.onnx.export(
            model,
            (torch.zeros(1, SEGMENT),),
            out,
            input_names=["audio"],
            output_names=["onset", "offset", "frame", "velocity"],
            opset_version=17,
            dynamo=False,
        )
    print("exported", out)


if __name__ == "__main__":
    main()
