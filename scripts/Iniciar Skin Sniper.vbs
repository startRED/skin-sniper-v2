' Abre o Skin Sniper sem janela preta e mostra o painel no navegador.
' Fica na mesma pasta do skin-sniper.exe. Se ele já estiver rodando, o segundo
' se recusa a subir (porta ocupada) e só o navegador abre.
Set fso = CreateObject("Scripting.FileSystemObject")
Set sh = CreateObject("WScript.Shell")
pasta = fso.GetParentFolderName(WScript.ScriptFullName)
sh.CurrentDirectory = pasta
sh.Run """" & pasta & "\skin-sniper.exe""", 0, False
WScript.Sleep 1500
sh.Run "http://localhost:8788", 1, False
