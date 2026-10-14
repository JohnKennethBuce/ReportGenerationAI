@echo off
cd /d "%~dp0"
set PATH=%USERPROFILE%\.cargo\bin;%PATH%

if exist "%~dp0DayRecap.exe" (
    start "" "%~dp0DayRecap.exe"
    exit /b 0
)
if exist "%~dp0target\release\dayrecap.exe" goto run_release
if exist "%~dp0target\debug\dayrecap.exe" goto run_debug

echo Building DayRecap first...
cargo build
goto run_debug

:run_release
echo Starting DayRecap in System Tray and opening Chatbox...
start "" "%~dp0target\release\dayrecap.exe"
exit /b 0

:run_debug
echo Starting DayRecap in System Tray and opening Chatbox...
start "" "%~dp0target\debug\dayrecap.exe"
exit /b 0
