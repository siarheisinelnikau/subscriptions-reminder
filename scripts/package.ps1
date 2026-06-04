$ErrorActionPreference = 'Stop'
$metadata = cargo metadata --no-deps --format-version 1 --offline | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'Cannot read Cargo metadata' }
$version = ($metadata.packages | Where-Object { $_.name -eq 'subscriptions-reminder' }).version
$name = "SubscriptionsReminder-v$version.exe"
New-Item -ItemType Directory -Path 'dist' -Force | Out-Null
Copy-Item -LiteralPath 'target/release/subscriptions-reminder.exe' -Destination "dist/$name" -Force
$hash = (Get-FileHash -LiteralPath "dist/$name" -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText((Join-Path $PWD 'dist/SHA256SUMS.txt'), "$hash  $name`n", [Text.UTF8Encoding]::new($false))
Write-Output "Packaged dist/$name"
