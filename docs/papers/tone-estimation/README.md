# Literature review: estimating the guitar tone of a commercial recording

IEEEtran conference-format review of the work behind a planned tone estimator for the app:
effect recognition and parameter estimation, amplifier and effect modelling (paired, unpaired
and grey-box), effect removal, audio-effect style transfer and inference-time optimisation,
effect representations, synthesizer sound matching, datasets and metrics, and what commercial
products and open-source tools do. Section 10 turns it into an approach for this app.

- `main.tex`: title, abstract, section includes, bibliography
- `sections/`: one file per section
- `references.bib`: bibliography (metadata checked against arXiv, Crossref and publisher pages
  on 2026-11-25; product pages and terms accessed the same day)

The companion review on tablature transcription is `../tablature-transcription/`.

Build (needs `pdflatex`, `bibtex`, `latexmk` and the `IEEEtran` class, e.g. Ubuntu's
`texlive-publishers`):

```bash
latexmk -pdf main.tex   # writes build/main.pdf (see .latexmkrc)
```
