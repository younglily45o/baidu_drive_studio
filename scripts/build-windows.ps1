$ErrorActionPreference = "Stop"
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    cargo build --locked --release
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed" }
    $output = Join-Path $PWD "dist\Baidu-Drive-Studio-Windows-x64"
    New-Item -ItemType Directory -Force -Path $output | Out-Null
    Copy-Item "target\release\Baidu-Drive-Studio.exe" $output -Force
    Copy-Item "README.md", "LICENSE", "THIRD-PARTY-NOTICES.md", "DEPENDENCY-LICENSES.txt" $output -Force
    Copy-Item "assets\FONT-LICENSE.txt" $output -Force
    New-Item -ItemType Directory -Force -Path (Join-Path $output "docs") | Out-Null
    Copy-Item "docs\verification.md", "docs\direct-link-feasibility.md" (Join-Path $output "docs") -Force
    $names = @("Baidu-Drive-Studio.exe", "README.md", "LICENSE", "THIRD-PARTY-NOTICES.md", "DEPENDENCY-LICENSES.txt", "FONT-LICENSE.txt", "docs/verification.md", "docs/direct-link-feasibility.md")
    $sums = foreach ($name in ($names | Sort-Object)) {
        $hash = (Get-FileHash (Join-Path $output $name) -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $name"
    }
    $sums | Set-Content (Join-Path $output "SHA256SUMS.txt") -Encoding UTF8
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archivePath = Join-Path $PWD "dist\Baidu-Drive-Studio-Windows-x64.zip"
    if (Test-Path $archivePath) { Remove-Item $archivePath }
    $archive = [System.IO.Compression.ZipFile]::Open($archivePath, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($name in @($names + "SHA256SUMS.txt")) {
            [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, (Join-Path $output $name), ("Baidu-Drive-Studio-Windows-x64/" + $name)) | Out-Null
        }
    } finally { $archive.Dispose() }
    Get-FileHash "dist\Baidu-Drive-Studio-Windows-x64.zip" -Algorithm SHA256
} finally { Pop-Location }
