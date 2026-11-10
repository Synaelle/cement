<p align="center">
  <img src="meta/readme.png" alt="i eat cement">
</p>

<h1 align="center">Cement</h1>

<p align="center">FTP server, file browser and CIA installer for your 3DS</p>

## Use it

1. Install `Cement.cia` from the [latest release](https://github.com/Synaelle/cement/releases/latest),
   or scan this in FBI (Remote Install, then Scan QR Code):

   <img src="meta/qr.png" width="160" alt="QR code for Cement.cia">

2. Open it and connect to the address shown on the top screen (like `192.168.1.10:5000`) with
   any FTP client, or with [Baguette](https://github.com/Synaelle/baguette).

## On the 3DS

L and R switch between the three tabs of the bottom screen.

Recent lists the CIAs you sent and the ones in `/`, `/cia`, `/cias` and `/fbi`. Browse opens every
folder of the SD card, and A on a PNG, JPEG or BMP image (like Luma's screenshots) shows it on the
top screen. Settings holds the FTP login, the FTP server switch, the CIA details on the top screen,
file protection, the screens, auto off, the look and the red look's colour. Red is the default
look, with Miku teal and the sharper Ficelle next to it. Settings also shows when a newer Cement is
out, and A installs it. The top screen explains the selected setting.

The top screen shows the selected CIA's icon, title and version, and whether it's new or an update.
A installs it and deletes the file once it's installed. A reinstall, a downgrade or a system title
asks for a second press of A first. A PC can't install system titles, and a reinstall or downgrade
it starts waits for A on the 3DS. X deletes the selected file or folder when pressed twice. While
"Protect important 3DS files" is on, `/Nintendo 3DS`, `/boot.firm`, `/luma`, `/gm9` and
`/boot.3dsx` can't be deleted.

With no login set, anyone on your Wi-Fi can connect. The password isn't kept on the SD card, only a
salted hash of it.

## Baguette

[Baguette](https://github.com/Synaelle/baguette) is the PC app made for Cement. It shows
transfer progress and the 3DS battery, installs CIAs from the PC and sends many small files much
faster. The colour picked in either app shows in both, and the last one picked wins. Cement works
fine with any other FTP client too.

## Build

On Linux or WSL, with devkitPro (or Docker), `makerom`, `bannertool` and libclang:

```sh
rustup toolchain install nightly-2026-11-09 -c rust-src
cargo install --locked cargo-3ds --version 0.1.5
./build.sh
```

`./build.sh fast` builds quicker to try a change (no LTO, always a dev version).

## Contributing

Contributions are welcome, as long as they stay within what Cement is for (a fast FTP server and
CIA installer for the 3DS) and don't break the SITE commands that PC apps like Baguette use. A
change to them needs a new protocol level. For something bigger, open an issue first to talk it
through.

## Credits

The fonts are [Fredoka](https://github.com/hafontia/Fredoka-One),
[Nunito](https://github.com/googlefonts/nunito) and
[Hanken Grotesk](https://github.com/marcologous/hanken-grotesk), under the SIL Open Font License.
The button glyphs are Kenney's [Input Prompts](https://kenney.nl/assets/input-prompts) (CC0).

## License

AGPL-3.0 or later, see [LICENSE](LICENSE). The CIA installer follows
[FBI](https://github.com/Steveice10/FBI) (MIT), see [NOTICE](NOTICE).
