$env:RUSTUP_HOME = "D:\rustup"
$env:CARGO_HOME = "D:\cargo"
if (!(Test-Path "D:\tmp")) { New-Item -ItemType Directory -Force -Path "D:\tmp" | Out-Null }
$env:TEMP = "D:\tmp"
$env:TMP = "D:\tmp"
$env:CARGO_INCREMENTAL = "0"
$env:PATH = "D:\cargo\bin;C:\Users\thega\.cargo\bin;$env:PATH"
& "C:\Users\thega\.cargo\bin\cargo.exe" @args
