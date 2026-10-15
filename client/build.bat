@echo off
rem Builds Syna3DSTools.exe with MinGW-w64 (g++ and windres on PATH)
cd /d "%~dp0"
windres src\Syna3DSTools.rc -O coff -o Syna3DSTools_res.o || exit /b 1
g++ -specs=src/nomanifest.specs -std=c++17 -O2 -s -static -municode -mwindows -DUNICODE -D_UNICODE -D_WIN32_WINNT=0x0A00 -DWINVER=0x0A00 ^
    src\main.cpp Syna3DSTools_res.o -o Syna3DSTools.exe ^
    -lws2_32 -lcomctl32 -lole32 -luuid -lshell32 -lshlwapi -luxtheme -ldwmapi -lgdi32 -luser32 || exit /b 1
del Syna3DSTools_res.o
echo built Syna3DSTools.exe
