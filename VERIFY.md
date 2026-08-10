# Verify Your Download

Every Mnemosyne release includes a SHA-256 checksum file (`.sha256`) for each binary and a
`checksums.txt` aggregating all hashes.

## Linux / macOS

```sh
# Compare the binary against its published checksum:
sha256sum -c mnemosyne-linux-x86_64-gnu.sha256

# Or verify all checksums at once:
sha256sum -c checksums.txt
```

On macOS, replace `sha256sum` with `shasum -a 256`.

## Windows (PowerShell)

```powershell
$expected = (Get-Content mnemosyne-windows-x86_64.exe.sha256).Split(' ')[0]
$actual   = (Get-FileHash -Algorithm SHA256 mnemosyne-windows-x86_64.exe).Hash.ToLower()
if ($expected -eq $actual) { "OK" } else { "MISMATCH" }
```

If the hash doesn't match, delete the binary and re-download — the file may be
corrupted or tampered with.
