' Desk-Remote TEST agent launcher: no console window (desk-service.exe is a GUI-subsystem binary).
' Extra command-line arguments are appended (e.g. --loopback --port 47100).
' Stop with DURDUR.vbs. Logs go to data\system\logs.
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
here = fso.GetParentFolderName(WScript.ScriptFullName)
sh.CurrentDirectory = here
extra = ""
For Each a In WScript.Arguments
  extra = extra & " " & a
Next
sh.Run """" & here & "\desk-service.exe"" run --console --data-dir """ & here & "\data""" & extra, 0, False
