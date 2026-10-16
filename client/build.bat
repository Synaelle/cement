@echo off
rem Builds Syna3DSTools.exe (GUI) and syna3ds.exe (command line, for agents and
rem scripts) with MinGW-w64 (g++ and windres on PATH)
cd /d "%~dp0"

rem version: <..\VERSION>.<commit count>-<short commit>, plus -dev with uncommitted changes
set /p BASEVER=<..\VERSION
set BUILDNO=
set COMMIT=
set DIRTY=
for /f %%c in ('git rev-list --count HEAD 2^>nul') do set BUILDNO=%%c
for /f %%h in ('git rev-parse --short HEAD 2^>nul') do set COMMIT=%%h
for /f "delims=" %%d in ('git status --porcelain 2^>nul') do set DIRTY=-dev
if "%BUILDNO%"=="" (set APPVER=%BASEVER%-dev) else (set APPVER=%BASEVER%.%BUILDNO%-%COMMIT%%DIRTY%)
echo version %APPVER%

windres src\Syna3DSTools.rc -O coff -o Syna3DSTools_res.o || exit /b 1
g++ -specs=src/nomanifest.specs -std=c++17 -O2 -s -static -municode -mwindows -DUNICODE -D_UNICODE -D_WIN32_WINNT=0x0A00 -DWINVER=0x0A00 -DAPP_VERSION=\"%APPVER%\" ^
    src\main.cpp Syna3DSTools_res.o -o Syna3DSTools.exe ^
    -lws2_32 -lcomctl32 -lole32 -luuid -lshell32 -lshlwapi -luxtheme -ldwmapi -lgdi32 -luser32 || exit /b 1
del Syna3DSTools_res.o
echo built Syna3DSTools.exe v%APPVER%
g++ -std=c++17 -O2 -s -static -municode -DUNICODE -D_UNICODE -D_WIN32_WINNT=0x0A00 -DWINVER=0x0A00 -DAPP_VERSION=\"%APPVER%\" ^
    src\cli.cpp -o syna3ds.exe -lws2_32 -lole32 -luuid -lshell32 || exit /b 1
echo built syna3ds.exe v%APPVER%
