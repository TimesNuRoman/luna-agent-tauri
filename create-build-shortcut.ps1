$ErrorActionPreference = 'Stop'

$projectDir   = 'D:\Code\LunaAgent\luna-agent-tauri'
$wrapperPath  = Join-Path $projectDir 'tauri-build.cmd'
$desktopPath  = [Environment]::GetFolderPath('Desktop')
$shortcutPath = Join-Path $desktopPath 'Luna Agent - Tauri Build.lnk'

$shell    = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)

$shortcut.TargetPath       = $wrapperPath
$shortcut.WorkingDirectory = $projectDir
$shortcut.WindowStyle      = 1
$shortcut.IconLocation     = "$env:SystemRoot\System32\shell32.dll,12"
$shortcut.Description      = 'Luna Agent - Tauri build (release exe)'

$shortcut.Save()

Write-Output ('OK: ' + $shortcutPath)
