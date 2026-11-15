# Measures memory across open / scroll / hide rounds without anyone at the keyboard: the build's
# soak mode (src/app/soak.rs) opens the picker off-screen without taking focus, scrolls Discord
# imports for 40 s, hides it for 30 s, and repeats. Uses the real library; quit the installed gifly
# first, or the test build just opens that one and exits.
#
# Build:  cargo build --release --config profile.release.debug-assertions=true
# Run:    scripts/soak.ps1 -Name before [-Rounds 4] [-HoverPlay]
# Writes <Name>.csv (samples every ~2.5 s) and <Name>.log (the build's log, with "soak" steps) to
# %TEMP%\gifly-soak, then prints each phase's end and peak, and the CPU time used so far.
param([Parameter(Mandatory)][string]$Name, [int]$Rounds = 4, [switch]$HoverPlay)

$dir = New-Item -ItemType Directory -Force "$env:TEMP\gifly-soak"
$csv, $log = "$dir\$Name.csv", "$dir\$Name.log"
$env:GIFLY_SOAK = "$Rounds"
$env:GIFLY_SOAK_HOVER = if ($HoverPlay) { "1" } else { $null }
$p = Start-Process "$PSScriptRoot\..\target\release\gifly.exe" -ArgumentList "--hidden" -WindowStyle Hidden `
    -RedirectStandardError $log -RedirectStandardOutput "$dir\$Name.out" -PassThru
Start-Sleep -Seconds 1

"ms,private_mb,ws_mb,gpu_ded_mb,cpu_s" | Set-Content -Encoding utf8 $csv
while (-not $p.HasExited) {
    $p.Refresh()
    $ded = (Get-Counter "\GPU Process Memory(pid_$($p.Id)*)\Dedicated Usage" -ErrorAction SilentlyContinue).CounterSamples |
        Measure-Object CookedValue -Sum
    "{0},{1:F0},{2:F0},{3:F0},{4:F1}" -f [DateTimeOffset]::Now.ToUnixTimeMilliseconds(), ($p.PrivateMemorySize64 / 1MB),
        ($p.WorkingSet64 / 1MB), ($ded.Sum / 1MB), $p.TotalProcessorTime.TotalSeconds | Add-Content -Encoding utf8 $csv
    Start-Sleep -Milliseconds 1500
}

# Each sample belongs to the last soak step before it.
$steps = Get-Content $log | Where-Object { $_ -like 'soak *' } | ForEach-Object {
    $f = $_ -split ' '; [pscustomobject]@{ Ms = [long]$f[1]; Phase = "$($f[2]) $($f[3])" }
}
Import-Csv $csv | Group-Object { $ms = [long]$_.ms; ($steps | Where-Object Ms -le $ms | Select-Object -Last 1).Phase } |
    ForEach-Object {
        $last = $_.Group[-1]
        [pscustomobject]@{
            Phase = $_.Name; Private = $last.private_mb; WorkingSet = $last.ws_mb; Gpu = $last.gpu_ded_mb
            PeakPrivate = ($_.Group.private_mb | Measure-Object -Maximum).Maximum
            PeakGpu = ($_.Group.gpu_ded_mb | Measure-Object -Maximum).Maximum
            CpuSeconds = $last.cpu_s
        }
    } | Format-Table -AutoSize
