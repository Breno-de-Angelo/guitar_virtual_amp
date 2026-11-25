# Literature review: automatic guitar tablature transcription

IEEEtran conference-format review of the literature behind `guitar_core/src/transcribe/`:
datasets, metrics, modular pipelines (separation, note transcription, rhythm, fingering,
techniques), end-to-end audio-to-tablature models, and open problems. The research plan that
follows from it is `../transcription_roadmap.md`.

- `main.tex`: title, abstract, section includes, bibliography
- `sections/`: one file per section
- `references.bib`: bibliography (arXiv metadata checked on 2026-11-24)

Build (needs `pdflatex`, `bibtex`, `latexmk` and the `IEEEtran` class, e.g. Ubuntu's
`texlive-publishers`):

```bash
latexmk -pdf main.tex   # writes build/main.pdf (see .latexmkrc)
```
