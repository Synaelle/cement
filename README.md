# Cement

![Cement](server/meta/banner.png)

Fast file transfers between Windows and a Nintendo 3DS.

- **3DS app** (`Cement.cia`): FTP server for the SD card. Uploaded `.cia` files show up on the bottom screen, press A to install them. Closing the lid puts the 3DS to sleep.
- **Windows app** (`Cement.exe`): file explorer with drag and drop, auto-reconnect, resumable transfers, Send to PC and dark mode.
- **Command line** (`cement-cli.exe`): `status`, `ls`, `put`, `get`, `rm`, `mkdir`, `mv`. See `cement-cli.exe --help`.

## Build

3DS, with devkitPro (devkitARM, libctru), makerom and bannertool:

```
cd server
make
```

Windows, with MinGW-w64:

```
client\build.bat
```

## License

MIT, like [FBI](https://github.com/Steveice10/FBI). See [LICENSE](LICENSE).
