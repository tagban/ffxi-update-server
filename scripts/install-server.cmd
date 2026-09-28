@echo off
rem Sets this PC up as an update server: runs install-server.ps1 beside this file (it asks for administrator rights).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install-server.ps1" %*
