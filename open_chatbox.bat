@echo off
cd /d "%~dp0"
set PATH=%USERPROFILE%\.cargo\bin;%PATH%

if exist "%~dp0DayRecap.exe" (
    start "" "%~dp0DayRecap.exe" --chat
    exit /b 0
)
if exist "%~dp0target\release\dayrecap.exe" goto chat_release
if exist "%~dp0target\debug\dayrecap.exe" goto chat_debug

echo Building DayRecap first...
cargo build
goto chat_debug

:chat_release
start "" "%~dp0target\release\dayrecap.exe" --chat
exit /b 0

:chat_debug
start "" "%~dp0target\debug\dayrecap.exe" --chat
exit /b 0
