# Standard license texts

`scripts/sbom.py` puts these texts in the release archive's
`THIRD-PARTY-LICENSES.txt` for a shipped crate that ships no license file of its
own, under the license it declares (ADR 0025, decision 1). The MIT text's
copyright line is filled with the crate's authors.

They are copied unchanged from the SPDX License List data,
[`spdx/license-list-data`](https://github.com/spdx/license-list-data) tag
`v3.29.0` (commit `31ba1a50e5397e00a304dbadc76531740e89ee48`), `text/<id>.txt`:

| File | SHA-256 |
| --- | --- |
| `Apache-2.0.txt` | `074e6e32c86a4c0ef8b3ed25b721ca23aca83df277cd88106ef7177c354615ff` |
| `CC0-1.0.txt` | `a2010f343487d3f7618affe54f789f5487602331c0a8d03f49e9a7c547cf0499` |
| `MIT.txt` | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |

A crate without license files that declares no license here makes
`sbom.py --check` fail until its text is added the same way.
