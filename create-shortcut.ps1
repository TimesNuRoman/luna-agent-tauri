$ErrorActionPreference = 'Stop'

$projectDir  = 'D:\Code\LunaAgent\luna-agent-tauri'
$wrapperPath = Join-Path $projectDir 'tauri-dev.cmd'
$desktopPath = [Environment]::GetFolderPath('Desktop')
$shortcutPath = Join-Path $desktopPath 'Luna Agent - Tauri Dev.lnk'

$shell    = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)

$shortcut.TargetPath       = $wrapperPath
$shortcut.WorkingDirectory = $projectDir
$shortcut.WindowStyle      = 1
$shortcut.IconLocation     = "$env:SystemRoot\System32\shell32.dll,12"
$shortcut.Description      = 'Luna Agent - Tauri dev (frontend + Rust backend)'

$shortcut.Save()

Write-Output ('OK: ' + $shortcutPath)
