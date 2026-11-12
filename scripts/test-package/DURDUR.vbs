' Stops the Desk-Remote TEST agent started by BASLAT.vbs (no console window).
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
here = fso.GetParentFolderName(WScript.ScriptFullName)
sh.Run """" & here & "\desk-service.exe"" stop", 0, True
