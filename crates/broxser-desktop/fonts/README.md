# Bundled fonts

`broxser-desktop` compiles these static Geist faces into its binary and
registers them with GPUI at startup (ADR 0027). Without them GPUI falls back
to the system's faces and the shell still works.

| File | Face | Font version |
| --- | --- | --- |
| `Geist-Regular.ttf` | Geist 400 | 1.800 |
| `Geist-Medium.ttf` | Geist 500 | 1.800 |
| `Geist-SemiBold.ttf` | Geist 600 | 1.800 |
| `GeistMono-Regular.ttf` | Geist Mono 400 | 1.700 |
| `GeistMono-Medium.ttf` | Geist Mono 500 | 1.700 |

Source: `fonts/Geist/ttf` and `fonts/GeistMono/ttf` of
[vercel/geist-font](https://github.com/vercel/geist-font) at commit
`10dc7658f13c38a474cde201bb09a4617267545b`, copied unchanged. `OFL.txt` is that
repository's license: SIL Open Font License 1.1, which allows bundling the fonts
with software, including in binary form, when the license travels with them.
`scripts/package.sh` puts it in the release archive as `licenses/geist-OFL.txt`.

`fonts.json` pins each file's SHA-256; `scripts/sbom.py` refuses a file that no
longer matches and lists the fonts in the SBOM. Update the fonts deliberately:
copy the new files from a reviewed upstream commit, then update `fonts.json`,
this table and the NOTICE.
