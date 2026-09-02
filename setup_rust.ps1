$env:RUSTUP_HOME = "D:\rustup"
$env:CARGO_HOME = "D:\cargo"
Write-Host "Cleaning temp and cargo caches on C:..."
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue "C:\Users\thega\.rustup\downloads"
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue "C:\Users\thega\.cargo\registry\cache"
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue "C:\Users\thega\AppData\Local\Temp\*"

Write-Host "Installing stable toolchain into D:\rustup..."
& "C:\Users\thega\.cargo\bin\rustup.exe" toolchain install stable --profile default
