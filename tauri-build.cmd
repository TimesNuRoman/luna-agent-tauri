@echo off
REM Wrapper for `npm run tauri:build` that fixes the PATH for npm, cargo, and cmake
REM (they live in non-default locations on this machine; cmake is needed by
REM whisper-rs-sys build-script during `tauri build`).

set "PATH=C:\Program Files\nodejs;C:\Program Files\CMake\bin;C:\Users\dredf\.cargo\bin;%PATH%"
cd /d "%~dp0"
npm run tauri:build
