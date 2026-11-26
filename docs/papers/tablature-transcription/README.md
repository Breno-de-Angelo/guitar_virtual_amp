# Automatic guitar tablature transcription: study notes

A one-column, tutorial-style document about turning a song recording into a guitar tab: the
background (audio, machine learning, metrics), the state of the art stage by stage (datasets,
separation, note transcription, rhythm, fingering, techniques, end-to-end models), and exactly
what `guitar_core/src/transcribe/` does, with every benchmark result and its protocol. Numbers
are tagged MEASURED (run in this repository), REPORTED (stated by a paper) or EXPECTED; parts of
the app BUILT, IN PROGRESS or PLANNED. The detailed research log is
`../../transcription_roadmap.md`; the companion document on tone estimation is
`../tone-estimation/`.

- `main.tex`: title, abstract, parts, bibliography
- `sections/`: `00-about` (how to read, summary table), `01`-`04` background, `05`-`11` the
  pipeline stage by stage, `12`-`14` the app, benchmarks and assessment, `A`/`B` glossary and
  code map
- `../common/preamble.tex`: layout, coloured boxes and tags shared with the tone document
- `references.bib`: bibliography (arXiv metadata checked on 2026-11-24)

Update the "In Guitar Virtual Amp" boxes and Part III when the code or the benchmark numbers
change.

Build (needs `pdflatex`, `bibtex`, `latexmk` and TeX Live's `tcolorbox`, `pgfplots`, `natbib`):

```bash
latexmk -pdf main.tex   # writes build/main.pdf (see .latexmkrc)
```
