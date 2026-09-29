$ErrorActionPreference = 'Stop'
"LIBCLANG_PATH=C:\Program Files\LLVM\bin" >> $env:GITHUB_ENV
$sdk = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Directory |
    Where-Object { Test-Path (Join-Path $_.FullName 'x64\rc.exe') } |
    Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
if (-not $sdk) { throw 'Windows SDK resource compiler not found' }
"RC_EXE=$($sdk.FullName)\x64\rc.exe" >> $env:GITHUB_ENV
