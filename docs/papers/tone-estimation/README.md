# Estimating the guitar tone of a recording: study notes

A one-column, tutorial-style document about estimating a recording's guitar tone as settings of
the app's pedals: the background (signal chains, LTI filters and impulse responses,
nonlinearities, distances between sounds, CMA-ES), the literature (datasets and licences,
effect recognition, amplifier modelling, working without the dry signal, style transfer and
sound matching, products), and exactly what `guitar_core/src/tone/` does, with the formulas the
code uses and the benchmark results. Numbers are tagged MEASURED, REPORTED or EXPECTED; parts of
the app BUILT, IN PROGRESS or PLANNED. The companion document on tablature transcription is
`../tablature-transcription/`.

- `main.tex`: title, abstract, parts, bibliography
- `sections/`: `00-about` (how to read, summary table), `01`-`04` background, `05`-`11` the
  literature, `12`-`15` the app's design, implementation, results and next steps, `A`/`B`
  glossary and code map
- `../common/preamble.tex`: layout, coloured boxes and tags shared with the tablature document
- `references.bib`: bibliography (metadata checked against arXiv, Crossref and publisher pages
  on 2026-11-25; product pages and terms accessed the same day)

Update Section 13 (implementation) and Section 14 (results) when `tone/` or `eval_tone` change.

Build (needs `pdflatex`, `bibtex`, `latexmk` and TeX Live's `tcolorbox`, `pgfplots`, `natbib`):

```bash
latexmk -pdf main.tex   # writes build/main.pdf (see .latexmkrc)
```
