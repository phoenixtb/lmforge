@echo off
setlocal EnableExtensions

set "MISE=C:\Users\aist1-windows\AppData\Local\Microsoft\WinGet\Packages\jdx.mise_Microsoft.Winget.Source_8wekyb3d8bbwe\mise\bin\mise.exe"
set "VCVARS=C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"

if not exist "%MISE%" (
  echo mise not found at %MISE%
  exit /b 1
)
if not exist "%VCVARS%" (
  echo vcvars64.bat not found. Install VS Build Tools with "Desktop development with C++".
  exit /b 1
)

call "%VCVARS%" >nul
if errorlevel 1 (
  echo Failed to initialize MSVC environment.
  exit /b 1
)

where link.exe >nul 2>&1
if errorlevel 1 (
  echo link.exe still not on PATH after vcvars64. C++ workload may be incomplete.
  exit /b 1
)

pushd "%~dp0..\..\ui"
if errorlevel 1 (
  echo Could not cd to ui directory.
  exit /b 1
)
set "CARGO_TARGET_DIR=%~dp0..\..\target"
if not exist node_modules (
  if exist package-lock.json (
    call "%MISE%" exec -- npm ci
  ) else (
    call "%MISE%" exec -- npm install
  )
  if errorlevel 1 exit /b 1
)

call "%MISE%" exec -- npm run tauri build
set "BUILD_RC=%ERRORLEVEL%"
popd
if not "%BUILD_RC%"=="0" exit /b 1

echo.
echo Build OK.
set "TARGET=%~dp0..\..\target"
echo   exe:  %TARGET%\release\lmforge-ui.exe
for /r "%TARGET%\release\bundle\nsis" %%f in (*.exe) do echo   nsis: %%f
