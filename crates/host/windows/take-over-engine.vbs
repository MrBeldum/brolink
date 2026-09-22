' Runs the engine take-over script with no console window. The logon task
' runs in the user's desktop session, where a .cmd or a plain powershell.exe
' would flash a terminal; wscript is a windowed host, so nothing shows.
Set shell = CreateObject("WScript.Shell")
script = shell.ExpandEnvironmentStrings("%LOCALAPPDATA%") & "\BroLink\take-over-engine.ps1"
shell.Run "powershell.exe -NoProfile -ExecutionPolicy Bypass -File """ & script & """", 0, True
