# OS/2 Warp 4 (1996) original references

These are original operating-system captures and IBM bitmap resources, not
modern themes or ChonkStep output. `sources.json` pins source URLs, the font
archive commit, and SHA-256 hashes. The historical test oracle must never be
regenerated from the renderer.

* `editor.png`, `desktop.png`, `empty.png`: Marcin Wichary's
  [GUIdebook OS/2 Warp 4 gallery](https://guidebookgallery.org/screenshots/os2warp4/).
  Unmodified downloads. The editor is the full active-frame and title-text
  oracle; Voice Manager in the desktop capture is the inactive-title oracle.
* `menus.png`: Nathan Lineback's
  [OS/2 Warp 4 tour](https://toastytech.com/guis/os242.html), unmodified
  `os24_warpbar.png`. This captures the original cascading WarpCenter menu,
  including its **Helv 8** text and isometric folder icon. Its display palette
  quantizes black/gray/blue to 8/206/173; tests explicitly map these to the
  GUIdebook capture's 0/207/170 where comparing colored icons.
* `bold.atlas`: Latin-1 subset of IBM's **WarpSans Bold 9, 96 dpi**, originally
  in `DSPRES.DLL`. `regular.atlas`: Latin-1 subset of **Helv 8, 96 dpi**.
  The human-readable YAFF transcriptions are preserved by Rob Hagemans'
  [hoard-of-bitfonts](https://github.com/robhagemans/hoard-of-bitfonts/tree/master/os-2/os2_warp4).
  The exact immutable URLs and input hashes are in `sources.json`.
* `os2-warp-4-{640,800,1024}.png`: lossless conversion of the three 256-color
  images inside IBM's original `\OS2\BITMAP\WARPD.BMP` bitmap array, from
  `OS2IMAGE/DISK_16/REQUIRED` on the archived English Warp 4 installation CD.
  The PNGs retain the original pixels, including the dithered texture and wave
  wordmark. They do not use an upscaled fan recreation or an AI reconstruction.

IBM's artwork, bitmap typefaces, and interface imagery retain their original
attribution and copyright; their inclusion does not relicense them under the
project's code license or the SIL OFL. The screenshots retain their respective
capture attribution. The compatible OFL Workplace Sans font was evaluated,
but its bitmap strokes differed from these captures and it is not used here.

## Reproduction

```sh
python3 scripts/os2warp-reference/generate_assets.py --check
```

For a full asset rebuild, download the two pinned YAFF files to a temporary
font directory, extract `WARPD.BMP` from the installation archive recorded in
`sources.json`, and run:

```sh
python3 scripts/os2warp-reference/generate_assets.py \
  --font-dir /tmp/os2-fonts --warpd /tmp/WARPD.BMP --check
```

The generator verifies source hashes before conversion. Omit `--check` to write
identical assets. Wallpaper conversion used Pillow 12.3.0; encoded PNG bytes can
vary with codec versions, so preserve the pinned files when only reviewing.

The PACK2 member was decoded using the independently published
[FTCOMP decoder by Dimitriy Ryazantcev](https://gist.github.com/DJm00n/a55fa145a5a0c6f0f6d3940e64de4c7f):
initialize `Mem` with `coldInit()`, then call `decodeMember` at the member's
`80 60 00 00 fT19` stream marker. That research tool and the operating system
installation media are not shipped in ChonkStep. The resulting bitmap-array
hash is pinned before its three 8-bit entries are converted.
