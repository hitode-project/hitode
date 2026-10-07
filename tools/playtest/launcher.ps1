# 试玩启动器：选择预设游戏或游戏 EXE、代码页和附加参数，然后用同目录的 adv-run.exe 启动。
# 预设来自同目录的 games.json（可选）；存档写入本目录 saves\，游戏目录本身不会被写入；
# 每次运行的日志在 logs\。
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()

$root = $PSScriptRoot
$runner = Join-Path $root 'adv-run.exe'
$state = Join-Path $root 'last-game.txt'
$catalog = Join-Path $root 'games.json'
if (-not (Test-Path $runner)) {
    [System.Windows.Forms.MessageBox]::Show("找不到 adv-run.exe：$runner", '试玩启动器') | Out-Null
    exit 1
}

# games.json: [{ "name", "exe", "codepage": 932|936, "args": "--d3d9-sm3", "note" }]
$presets = @()
if (Test-Path $catalog) {
    # Windows PowerShell emits a JSON array as one object; assigning first unrolls it.
    $parsed = Get-Content -LiteralPath $catalog -Raw -Encoding UTF8 | ConvertFrom-Json
    $presets = @($parsed)
}
$last = if (Test-Path $state) { @(Get-Content -LiteralPath $state -Encoding UTF8 -TotalCount 4) } else { @() }
$form = New-Object System.Windows.Forms.Form
$form.Text = 'ADV 试玩启动器'
$form.StartPosition = 'CenterScreen'
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.ClientSize = New-Object System.Drawing.Size(560, 290)
$form.Font = New-Object System.Drawing.Font('Microsoft YaHei UI', 9)

$presetLabel = New-Object System.Windows.Forms.Label
$presetLabel.Text = '预设游戏：'
$presetLabel.Location = New-Object System.Drawing.Point(16, 16)
$presetLabel.AutoSize = $true
$preset = New-Object System.Windows.Forms.ComboBox
$preset.DropDownStyle = 'DropDownList'
$preset.Location = New-Object System.Drawing.Point(16, 38)
$preset.Size = New-Object System.Drawing.Size(528, 24)
foreach ($game in $presets) { [void]$preset.Items.Add($game.name) }
[void]$preset.Items.Add('（手动选择 EXE）')
$note = New-Object System.Windows.Forms.Label
$note.Location = New-Object System.Drawing.Point(16, 68)
$note.Size = New-Object System.Drawing.Size(528, 36)
$note.ForeColor = [System.Drawing.Color]::DimGray

$label = New-Object System.Windows.Forms.Label
$label.Text = '游戏 EXE：'
$label.Location = New-Object System.Drawing.Point(16, 110)
$label.AutoSize = $true
$path = New-Object System.Windows.Forms.TextBox
$path.Location = New-Object System.Drawing.Point(16, 132)
$path.Size = New-Object System.Drawing.Size(440, 24)
$browse = New-Object System.Windows.Forms.Button
$browse.Text = '浏览…'
$browse.Location = New-Object System.Drawing.Point(464, 130)
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
$cpLabel.Location = New-Object System.Drawing.Point(16, 174)
$cpLabel.AutoSize = $true
$codepage = New-Object System.Windows.Forms.ComboBox
$codepage.DropDownStyle = 'DropDownList'
$codepage.Location = New-Object System.Drawing.Point(150, 170)
$codepage.Size = New-Object System.Drawing.Size(200, 24)
[void]$codepage.Items.Add('日文 (932)')
[void]$codepage.Items.Add('简体中文 (936)')
$argsLabel = New-Object System.Windows.Forms.Label
$argsLabel.Text = '附加参数：'
$argsLabel.Location = New-Object System.Drawing.Point(16, 210)
$argsLabel.AutoSize = $true
$extra = New-Object System.Windows.Forms.TextBox
$extra.Location = New-Object System.Drawing.Point(150, 206)
$extra.Size = New-Object System.Drawing.Size(394, 24)

$preset.Add_SelectedIndexChanged({
    if ($preset.SelectedIndex -lt $presets.Count) {
        $game = $presets[$preset.SelectedIndex]
        $path.Text = $game.exe
        $codepage.SelectedIndex = if ("$($game.codepage)" -eq '936') { 1 } else { 0 }
        $extra.Text = "$($game.args)"
        $note.Text = "$($game.note)"
    } else {
        $note.Text = '手动选择游戏 EXE；需要时在“附加参数”中填写 --d3d9-sm2 或 --d3d9-sm3 等选项。'
    }
})
$chosen = [Array]::IndexOf(@($presets | ForEach-Object { $_.name }), $(if ($last.Count -ge 4) { $last[3] } else { $null }))
$preset.SelectedIndex = if ($chosen -ge 0) { $chosen } elseif ($presets.Count -gt 0 -and $last.Count -eq 0) { 0 } else { $presets.Count }
if ($last.Count -ge 1) {
    $path.Text = $last[0]
    $codepage.SelectedIndex = if ($last.Count -ge 2 -and $last[1] -eq '936') { 1 } else { 0 }
    $extra.Text = if ($last.Count -ge 3) { $last[2] } else { '' }
} elseif ($codepage.SelectedIndex -lt 0) {
    $codepage.SelectedIndex = 0
}

$start = New-Object System.Windows.Forms.Button
$start.Text = '启动'
$start.Location = New-Object System.Drawing.Point(370, 246)
$start.Size = New-Object System.Drawing.Size(84, 30)
$start.DialogResult = 'OK'
$cancel = New-Object System.Windows.Forms.Button
$cancel.Text = '取消'
$cancel.Location = New-Object System.Drawing.Point(460, 246)
$cancel.Size = New-Object System.Drawing.Size(84, 30)
$cancel.DialogResult = 'Cancel'
$form.AcceptButton = $start
$form.CancelButton = $cancel
$form.Controls.AddRange(@($presetLabel, $preset, $note, $label, $path, $browse, $cpLabel, $codepage,
    $argsLabel, $extra, $start, $cancel))
if ($form.ShowDialog() -ne 'OK') { exit 0 }

$exe = $path.Text.Trim('"', ' ')
if (-not $exe -or -not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    [System.Windows.Forms.MessageBox]::Show('请选择一个存在的游戏 EXE。', '试玩启动器') | Out-Null
    exit 1
}
$cp = if ($codepage.SelectedIndex -eq 1) { '936' } else { '932' }
$options = $extra.Text.Trim()
$presetName = if ($preset.SelectedIndex -lt $presets.Count) { $presets[$preset.SelectedIndex].name } else { '' }
Set-Content -LiteralPath $state -Value @($exe, $cp, $options, $presetName) -Encoding UTF8

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
$command = "`"`"$runner`" `"$exe`" --audio --codepage $cp $options --data-dir `"$saves`" " +
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
