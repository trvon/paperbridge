# PDF extraction fixtures

Real arXiv preprints used by `tests/pdf_extraction.rs` to verify that the
paperseed full-text extractor handles production PDFs (subset Type 1 fonts,
Flate-compressed content streams, ligatures, non-ASCII author names).

## Files

| File | Source | License | Why |
|------|--------|---------|-----|
| `arxiv_1408_5939_planar_subgraphs.pdf` | [arXiv:1408.5939v2](https://arxiv.org/abs/1408.5939) — *Planar Induced Subgraphs of Sparse Graphs*, Borradaile/Eppstein/Zhu | arXiv non-exclusive license to distribute | Uses LaTeX/TeX subset CM fonts (`CBMXSR+CMBX10`, `DGAPDV+CMMI7`, `DGKNVQ+CMEX10`) — the hand-rolled extractor returned text with a space between every letter (`"G le n c o r a B o r r a d a ile"`). pdf-extract returns the real string. |
| `arxiv_1808_06100_polynomial_optimization.pdf` | [arXiv:1808.06100v6](https://arxiv.org/abs/1808.06100) — *On the solution existence and stability of polynomial optimization problems*, Vu Trung Hieu | arXiv non-exclusive license | Vietnamese author name + math fonts (`ANWDFI+CMSY10`, `APHKXY+MSAM10`). Same letter-spacing pathology in the legacy extractor; pdf-extract recovers the name. |

Both PDFs are kept under ~210 KB so the repo overhead is small. Provenance
preserved here; the tests assert specific phrases from the abstracts to keep
them tied to the real content rather than incidental layout.
