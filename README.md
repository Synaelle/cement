<p align="center">
  <img src="meta/readme.png" alt="i eat cement">
</p>

<h1 align="center">Cement</h1>

<p align="center">A fast FTP server for your 3DS's SD card 🧱</p>

> The names and the design are a work in progress, nothing here is final yet.

## Use it

1. Install `Cement.cia` from the [latest release](https://github.com/Synaelle/cement/releases/latest),
   or scan this in FBI (Remote Install, then Scan QR Code):

   <img src="meta/qr.png" width="160" alt="QR code for Cement.cia">

2. Open it and connect to the address shown on the top screen (like `192.168.1.10:5000`) with
   any FTP client, or with [**Baguette**](https://github.com/Synaelle/baguette) 🥖

## On the 3DS

L and R switch the bottom screen's tabs:

- **Recent**: the CIAs you sent, and those in `/`, `/cia`, `/cias` and `/fbi`
- **Browse**: every folder of the SD card. A on an image (PNG, JPEG or BMP, like Luma's
  screenshots) shows it on the top screen
- **Settings**: the look, FTP login, FTP server on or off, CIA details on the top screen, file
  protection, screens and auto off. The top screen tells what the selected one does

The first time it starts, Cement asks for a look: **Cement**, round with Miku teal, or **Ink**,
plain black and white.

The top screen shows the selected CIA's icon, title and version, and whether it's new or an update.
A installs it (and deletes the file once it's installed). X deletes the selected file or folder,
pressed twice. Like in Baguette, `/Nintendo 3DS`, `/boot.firm`, `/luma`, `/gm9` and `/boot.3dsx`
can't be deleted while "Protect important 3DS files" is on.

With no login set, anyone on your Wi-Fi can connect. The password isn't kept on the SD card, only a
salted hash of it.

## Baguette

[Baguette](https://github.com/Synaelle/baguette) is the Windows app made for Cement. It shows
transfer progress and the 3DS battery, installs CIAs from the PC and sends many small files much
faster. Cement works fine with any other FTP client too.

## Build

On Linux or WSL, with devkitPro (or Docker), `makerom`, `bannertool` and libclang:

```sh
rustup toolchain install nightly -c rust-src
cargo install --locked cargo-3ds
./build.sh
```

`./build.sh fast` builds quicker to try a change (no LTO, always a dev version).

## Contributing

Contributions are welcome, as long as they stay within what Cement is for (a fast FTP server and
CIA installer for the 3DS) and don't break how Baguette and Cement talk to each other. A change
to that protocol has to be made in both apps. For something bigger, open an issue first to talk it
through.

## Credits

Fonts: [Fredoka](https://github.com/hafontia/Fredoka-One),
[Nunito](https://github.com/googlefonts/nunito), [IBM Plex Mono](https://github.com/IBM/plex) and
Ink Sans, made from [IBM Plex Sans](https://github.com/IBM/plex) and renamed as its license asks
(all SIL Open Font License). Button glyphs:
[Input Prompts](https://kenney.nl/assets/input-prompts) by Kenney (CC0).

## License

GPL-3.0 or later, see [LICENSE](LICENSE). The CIA installer follows
[FBI](https://github.com/Steveice10/FBI) (MIT), see [NOTICE](NOTICE).
