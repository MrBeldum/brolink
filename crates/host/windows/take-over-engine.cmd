@echo off
rem A task registered by an older version still runs this file. Hand off to the
rem windowless launcher at once so this console is gone within a blink.
start "" /b wscript.exe //B //Nologo "%LOCALAPPDATA%\BroLink\take-over-engine.vbs"
