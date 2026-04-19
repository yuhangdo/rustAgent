@echo off
setlocal
powershell -ExecutionPolicy Bypass -File "%~dp0run-android-debug.ps1" %*
endlocal
