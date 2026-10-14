@echo off
cd /d "%~dp0"
echo Generating your active Word and Excel reports...
"%~dp0target\debug\dayrecap.exe" --rules today
echo.
echo Opening Reports folder...
explorer "%APPDATA%\DayRecap\reports"
