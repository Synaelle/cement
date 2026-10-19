<p align="center">
  <img src="meta/banner.png" alt="Cement">
</p>

<h1 align="center">Cement</h1>

<p align="center">A tiny, fast FTP server for your 3DS's SD card 🧱</p>

> The names and the design are a work in progress, nothing here is final yet.

## Use it

1. Install `Cement.cia` (or put `Cement.3dsx` in `/3ds/` for the Homebrew Launcher)
2. Open it: the top screen shows the address, like `192.168.1.10` port `5000`
3. Connect with any FTP client (FileZilla, WinSCP...), or with
   [**Chomp**](https://github.com/Synaelle/chomp) 🍪

Prefer plain console text? **Cement Text** (`Cement-Text.cia`) is the same app with the old
text screens, in red. Both can be installed side by side.

Cement is tested on a New 3DS. It should run on an Old 3DS too, but that hasn't been tried on real hardware yet.

## Chomp + Cement

[Chomp](https://github.com/Synaelle/chomp) is the Windows app made for Cement: an explorer
with drag and drop, resumable transfers, Send to PC, and a command line for scripts. It
uses a few extras Cement adds to FTP (`SITE STATUS`, `SITE BATCH`) to show progress, time
left, free space and the battery. Cement works fine without it, with any FTP client.

## Build

Cement is written in Rust ([ctru-rs](https://github.com/rust3ds/ctru-rs) over libctru).
On Linux or WSL:

```sh
rustup toolchain install nightly -c rust-src
cargo install --locked cargo-3ds
./build.sh
```

You also need `makerom`, `bannertool` and libclang, plus devkitPro or Docker (the script
copies devkitARM out of the official image). `cargo build --release` instead makes a PC
version that plays the 3DS (the SD card is a folder), handy for tests.

## Planned

Next release:

- A user name and password of your choice
- Unpacking the uncompressed bundles Chomp sends when there are many small files

Later:

- Updating from within the app
- Versions for other consoles

## Credits

The red and black look of Chomp and Cement was inspired by the [vedal.ai](https://vedal.ai/)
website. Nothing is taken from it, and this project isn't affiliated with Vedal.

The fonts are [Fredoka](https://github.com/hafontia/Fredoka-One) and
[Nunito](https://github.com/googlefonts/nunito) (SIL Open Font License), and the button glyphs
come from [Input Prompts](https://kenney.nl/assets/input-prompts) by Kenney (CC0). Thanks!

## License

GPL-3.0 or later, see [LICENSE](LICENSE). Versions before 1.2.32 were MIT.
The CIA installer follows [FBI](https://github.com/Steveice10/FBI) (MIT). Its notice and the
licenses of the libraries built into the 3DS app are in [NOTICE](NOTICE).
