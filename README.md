<p align="center">
  <img src="meta/readme.png" alt="i eat cement">
</p>

<h1 align="center">Cement</h1>

<p align="center">A fast FTP server for your 3DS's SD card 🧱</p>

> The names and the design are a work in progress, nothing here is final yet.

## Use it

1. Install `Cement.cia` from the [latest release](https://github.com/Synaelle/cement/releases/latest),
   or scan this in FBI (Remote Install, then Scan QR Code). You can also put `Cement.3dsx` in
   `/3ds/` for the Homebrew Launcher.

   <img src="meta/qr.png" width="160" alt="QR code for Cement.cia">

2. Open it: the top screen shows the address, like `192.168.1.10` port `5000`
3. Connect with any FTP client (FileZilla, WinSCP...), or with
   [**Chomp**](https://github.com/Synaelle/chomp) 🍪

Prefer plain console text? **Cement Text** (`Cement-Text.cia`) is the same app with the old
text screens, in red. Both can be installed side by side.

## Login

With no login set, anyone on your Wi-Fi can connect (any user name and password works).
To ask for a user name and password, press **Y** on the 3DS and type them, or set them from
Chomp. An empty user name goes back to no login. They're kept in
`sdmc:/3ds/cement/settings.ini` as plain text, so it keeps out the curious, not a determined
person with your SD card. FTP itself isn't encrypted either.

Cement is tested on a New 3DS. It should run on an Old 3DS too, but that hasn't been tried on real hardware yet.

## Chomp + Cement

[Chomp](https://github.com/Synaelle/chomp) is the Windows app made for Cement: an explorer
with drag and drop, resumable transfers, Send to PC, and a command line for scripts. It
uses a few extras Cement adds to FTP (`SITE STATUS`, `SITE BATCH`...) to show progress, time
left, free space and the battery, to install CIAs and set the login from the PC. When it sends
many small files at once, it packs them into one uncompressed bundle that Cement unpacks as it
arrives, which is much faster than one file at a time. Cement works fine without it, with any
FTP client.

Chomp and Cement have their own version numbers, so what tells which Chomp goes with which
Cement is the protocol level of these extras, shown next to the version on the 3DS top screen
and in Chomp. Cement 2.0 speaks protocol 3 (logins and bundles), and Chomp tells you when the
Cement on your 3DS is too old for it.

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

- Updating from within the app
- Versions for other consoles

## Credits

The fonts are [Fredoka](https://github.com/hafontia/Fredoka-One) and
[Nunito](https://github.com/googlefonts/nunito) (SIL Open Font License), and the button glyphs
come from [Input Prompts](https://kenney.nl/assets/input-prompts) by Kenney (CC0). Thanks!

## License

GPL-3.0 or later, see [LICENSE](LICENSE). Versions before 1.2.32 were MIT.
The CIA installer follows [FBI](https://github.com/Steveice10/FBI) (MIT). Its notice and the
licenses of the libraries built into the 3DS app are in [NOTICE](NOTICE).
