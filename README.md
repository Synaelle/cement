# synaftp

Minimal FTP pair for getting files onto a 3DS fast.

- `server/` — 3DS FTP server, built as a CIA. Exposes the **SD card only**.
- `client/` — Windows explorer-style FTP client (single static `.exe`).

## Using it

1. Install `server/synaftp.cia` with FBI (or any CIA installer) and launch it.
   The top screen shows the IP and port (5000).
2. Run `client/synaftp.exe`. On first run it asks for that IP and port once
   (saved in `%APPDATA%\synaftp\config.ini`; change it later with **Server...**).
3. Drag files/folders from Explorer into the window to upload them (drop on a
   folder row to upload into that folder). Drag items out to Explorer to
   download them, or right-click → **Download to...**.

Client details:
- The folder is re-listed every time you open one.
- If the 3DS can't be reached the client keeps retrying every 2 s; queued and
  interrupted transfers resume where they stopped (SIZE + REST).
  **Force reconnect** drops both connections and reconnects immediately.
- Keys: Enter open, Backspace up, F2 rename, Del delete, F5 refresh, Ctrl+A.
- Dark mode by default; **Light mode / Dark mode** toggles it.

3DS details:
- Close the lid: the 3DS really sleeps (Wi-Fi off, best battery). Connections
  are closed cleanly and the server comes back on its own when you open it;
  the client reconnects and resumes.
- The bottom screen backlight is always off. SELECT toggles the top screen.
  START exits.

## Performance notes

- Server: one non-blocking `poll()` loop for all sockets plus a dedicated SD
  thread, so receiving and writing to the SD card overlap (4 × 256 KB buffers
  per session). On New 3DS the SD thread runs on the extra core, and the CIA
  asks for 804 MHz + L2 cache.
- NDM is locked in infrastructure mode while serving, so StreetPass/SpotPass
  don't use the radio during transfers (same approach as ftpd).
- Client: two connections (browsing stays responsive during transfers),
  `TCP_NODELAY` on the control connection, 1 MB I/O chunks.

`RMD` on this server deletes a folder recursively (one command instead of one
per file). The server accepts any login (no password), like ftpd: use it on
your own network.

## Building

Server (devkitPro with devkitARM + libctru, plus `makerom` and `bannertool`):

```bash
cd server
make MAKEROM=/path/to/makerom BANNERTOOL=/path/to/bannertool
```

Client (MinGW-w64 g++ and windres on PATH):

```bat
client\build.bat
```

## Tests

`test/` builds the server's real `main.c` for Windows through a thin shim
(`test/shim/3ds.h`) that maps the libctru SD/socket calls onto a folder and
Winsock, so the server logic can be tested on a PC:

```bash
gcc -std=gnu11 -O2 -D_WIN32_WINNT=0x0A00 -Itest/shim server/source/main.c -o test/server_pc.exe -lws2_32 -lshell32
python test/test_server.py test/server_pc.exe
g++ -std=c++17 -O2 -DUNICODE -D_UNICODE -D_WIN32_WINNT=0x0A00 -Iclient/src test/test_client.cpp -o test/test_client.exe -lws2_32 -lcomctl32 -lole32 -luuid -lshell32 -lshlwapi -luxtheme -ldwmapi -lgdi32 -luser32
test/test_client.exe test/server_pc.exe
```
