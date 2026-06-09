@echo off
setlocal EnableExtensions

set "VCVARS=C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
set "PATH=%USERPROFILE%\.cargo\bin;C:\Program Files\Volta;%USERPROFILE%\AppData\Local\Volta\bin;%PATH%"

if not exist "%VCVARS%" (
  echo vcvars64.bat not found. Install VS Build Tools with "Desktop development with C++".
  exit /b 1
)

where npm >nul 2>&1
if errorlevel 1 (
  echo npm not found on PATH. Install Node via Volta: volta install node@lts
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
    call npm ci
  ) else (
    call npm install
  )
  if errorlevel 1 exit /b 1
)

call npm run tauri build
set "BUILD_RC=%ERRORLEVEL%"
popd
if not "%BUILD_RC%"=="0" exit /b 1

echo.
echo Build OK.
set "TARGET=%~dp0..\..\target"
echo   exe:  %TARGET%\release\lmforge-ui.exe
for /r "%TARGET%\release\bundle\nsis" %%f in (*.exe) do echo   nsis: %%f
