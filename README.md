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
   [**Baguette**](https://github.com/Synaelle/chomp) 🥖

## On the 3DS

The bottom screen has three tabs (switch with L and R, or touch them):

- **Recent**: the CIA files sent since Cement started, and those in `/`, `/cia`, `/cias` and
  `/fbi`. Press A to install one (the file is deleted once it's installed).
- **Browse**: the folders of the SD card, to install a CIA from anywhere. B goes back up.
- **Settings**: the colour (or let Baguette pick it), the FTP login, turning the FTP server off,
  keeping the screens on, and turning the 3DS off after a while without use.

**Cement Text**, the old console text version, is discontinued: 2.0.64 was its last release.

## Login

With no login set, anyone on your Wi-Fi can connect (any user name and password works).
To ask for a user name and password, set them in Settings on the 3DS (FTP login), or from
Baguette. An empty user name goes back to no login. They're kept in
`sdmc:/3ds/cement/settings.ini` as plain text, so it keeps out the curious, not a determined
person with your SD card. FTP itself isn't encrypted either.

Cement is tested on a New 3DS. It should run on an Old 3DS too, but that hasn't been tried on real hardware yet.

## Baguette + Cement

[Baguette](https://github.com/Synaelle/chomp) (formerly Chomp) is the Windows app made for Cement: an explorer
with drag and drop, resumable transfers, Send to PC, and a command line for scripts. It
uses a few extras Cement adds to FTP (`SITE STATUS`, `SITE BATCH`...) to show progress, time
left, free space and the battery, to install CIAs and set the login from the PC. When it sends
many small files at once, it packs them into one uncompressed bundle that Cement unpacks as it
arrives, which is much faster than one file at a time. Cement works fine without it, with any
FTP client.

Baguette and Cement have their own version numbers, so what tells which Baguette goes with which
Cement is the protocol level of these extras, shown next to the version on the 3DS top screen
and in Baguette. Cement 2.0 speaks protocol 3 (logins and bundles), and Baguette tells you when the
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

## Legacy version (C and C++)

Before 2.0, Cement was written in C (the 3DS app) and C++ (the Windows app and the command
line). That version is on the [legacy branch](../../tree/legacy), with its builds in the
[1.2.28 release](../../releases/tag/v1.2.28) and its own build steps in its README (devkitPro
for the 3DS app, MinGW-w64 or g++ for the PC apps). It's still supported, but it only gets bug
fixes: new features go to this version and to [Baguette](https://github.com/Synaelle/chomp).

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
