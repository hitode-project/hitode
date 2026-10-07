# 试玩启动器：选择游戏 EXE 和代码页，然后用同目录的 adv-run.exe 启动。
# 存档写入本目录 saves\，游戏目录本身不会被写入；每次运行的日志在 logs\。
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()

$root = $PSScriptRoot
$runner = Join-Path $root 'adv-run.exe'
$state = Join-Path $root 'last-game.txt'
if (-not (Test-Path $runner)) {
    [System.Windows.Forms.MessageBox]::Show("找不到 adv-run.exe：$runner", '试玩启动器') | Out-Null
    exit 1
}

$last = if (Test-Path $state) { (Get-Content -LiteralPath $state -Encoding UTF8 -TotalCount 2) } else { @() }
$form = New-Object System.Windows.Forms.Form
$form.Text = 'ADV 试玩启动器'
$form.StartPosition = 'CenterScreen'
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.ClientSize = New-Object System.Drawing.Size(520, 190)
$form.Font = New-Object System.Drawing.Font('Microsoft YaHei UI', 9)

$label = New-Object System.Windows.Forms.Label
$label.Text = '游戏 EXE：'
$label.Location = New-Object System.Drawing.Point(16, 20)
$label.AutoSize = $true
$path = New-Object System.Windows.Forms.TextBox
$path.Location = New-Object System.Drawing.Point(16, 42)
$path.Size = New-Object System.Drawing.Size(400, 24)
if ($last.Count -ge 1) { $path.Text = $last[0] }
$browse = New-Object System.Windows.Forms.Button
$browse.Text = '浏览…'
$browse.Location = New-Object System.Drawing.Point(424, 40)
$browse.Size = New-Object System.Drawing.Size(80, 27)
$browse.Add_Click({
    $dialog = New-Object System.Windows.Forms.OpenFileDialog
    $dialog.Filter = '游戏程序 (*.exe)|*.exe'
    if ($path.Text -and (Test-Path -LiteralPath $path.Text)) {
        $dialog.InitialDirectory = Split-Path -LiteralPath $path.Text
    }
    if ($dialog.ShowDialog($form) -eq 'OK') { $path.Text = $dialog.FileName }
})

$cpLabel = New-Object System.Windows.Forms.Label
$cpLabel.Text = '游戏语言（代码页）：'
$cpLabel.Location = New-Object System.Drawing.Point(16, 84)
$cpLabel.AutoSize = $true
$codepage = New-Object System.Windows.Forms.ComboBox
$codepage.DropDownStyle = 'DropDownList'
$codepage.Location = New-Object System.Drawing.Point(150, 80)
$codepage.Size = New-Object System.Drawing.Size(200, 24)
[void]$codepage.Items.Add('日文 (932)')
[void]$codepage.Items.Add('简体中文 (936)')
$codepage.SelectedIndex = if ($last.Count -ge 2 -and $last[1] -eq '936') { 1 } else { 0 }

$start = New-Object System.Windows.Forms.Button
$start.Text = '启动'
$start.Location = New-Object System.Drawing.Point(330, 140)
$start.Size = New-Object System.Drawing.Size(84, 30)
$start.DialogResult = 'OK'
$cancel = New-Object System.Windows.Forms.Button
$cancel.Text = '取消'
$cancel.Location = New-Object System.Drawing.Point(420, 140)
$cancel.Size = New-Object System.Drawing.Size(84, 30)
$cancel.DialogResult = 'Cancel'
$form.AcceptButton = $start
$form.CancelButton = $cancel
$form.Controls.AddRange(@($label, $path, $browse, $cpLabel, $codepage, $start, $cancel))
if ($form.ShowDialog() -ne 'OK') { exit 0 }

$exe = $path.Text.Trim('"', ' ')
if (-not $exe -or -not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    [System.Windows.Forms.MessageBox]::Show('请选择一个存在的游戏 EXE。', '试玩启动器') | Out-Null
    exit 1
}
$cp = if ($codepage.SelectedIndex -eq 1) { '936' } else { '932' }
Set-Content -LiteralPath $state -Value @($exe, $cp) -Encoding UTF8

# Each install gets its own save overlay: the EXE name plus a short path hash.
$sha = [System.Security.Cryptography.SHA256]::Create()
$hash = ($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($exe.ToLowerInvariant())) |
    Select-Object -First 4 | ForEach-Object { $_.ToString('x2') }) -join ''
$name = [System.IO.Path]::GetFileNameWithoutExtension($exe)
$saves = Join-Path $root "saves\$name-$hash"
$logs = Join-Path $root 'logs'
New-Item -ItemType Directory -Force -Path $saves, $logs | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$log = Join-Path $logs "$name-$stamp.log"
$report = Join-Path $logs "$name-$stamp.json"

# Game dialogs and message boxes are shown to the player, never answered for
# them: they can choose the window mode or other settings.
# No console window, but no show-window override either: SW_HIDE in the startup
# info would hide the game's first ShowWindow(SW_SHOWDEFAULT).
$command = "`"`"$runner`" `"$exe`" --audio --codepage $cp --data-dir `"$saves`" " +
    "--report `"$report`" 1> `"$log.out`" 2> `"$log`"`""
$info = New-Object System.Diagnostics.ProcessStartInfo
$info.FileName = $env:ComSpec
$info.Arguments = "/d /s /c $command"
$info.WorkingDirectory = $root
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$process = [System.Diagnostics.Process]::Start($info)
$process.WaitForExit()
if ($process.ExitCode -ne 0) {
    $tail = if (Test-Path $log) { (Get-Content -LiteralPath $log -Encoding UTF8 -Tail 8) -join "`n" } else { '' }
    [System.Windows.Forms.MessageBox]::Show("运行结束，退出码 $($process.ExitCode)。`n日志：$log`n`n$tail", '试玩启动器') | Out-Null
}
