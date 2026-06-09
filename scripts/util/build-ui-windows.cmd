@echo off
setlocal
set "PATH=%USERPROFILE%\.cargo\bin;C:\Program Files\Volta;%USERPROFILE%\AppData\Local\Volta\bin;%PATH%"
cd /d "%~dp0..\..\ui"
if not exist node_modules (
  call npm ci
)
call npm run tauri build
if errorlevel 1 (
  echo.
  echo Build failed. If you see "link.exe not found", install:
  echo   Visual Studio 2022 Build Tools - "Desktop development with C++"
  echo   https://visualstudio.microsoft.com/visual-cpp-build-tools/
  exit /b 1
)
echo.
echo NSIS installer:
dir /s /b "..\target\release\bundle\nsis\*.exe" 2>nul
