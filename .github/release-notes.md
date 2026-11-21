AIchemist Desktop Buddy {{VERSION}}.

> [!WARNING]
> **These builds are not code-signed or notarized.** Windows SmartScreen and macOS Gatekeeper
> will warn the first time you open them. Follow the
> [install instructions](https://github.com/{{REPOSITORY}}/blob/{{TAG}}/README.md#install) to get
> past the warning, and check the download against `SHA256SUMS.txt` first if you want to be sure
> it is the file built here.

## Downloads

| Platform | File |
|---|---|
| Windows 10/11 (x64), installer | `AIchemist-Desktop-Buddy_{{VERSION}}_x64-setup.exe` |
| Windows 10/11 (x64), MSI | `AIchemist-Desktop-Buddy_{{VERSION}}_x64_en-US.msi` |
| macOS (Apple silicon and Intel) | `AIchemist-Desktop-Buddy_{{VERSION}}_universal.dmg` |
| Linux (best-effort, untested) | `.deb` and `.AppImage`, when the Linux build succeeded |

Verify a download:

```sh
sha256sum -c SHA256SUMS.txt --ignore-missing          # Linux
shasum -a 256 -c SHA256SUMS.txt --ignore-missing      # macOS
```

On Windows: `Get-FileHash .\AIchemist-Desktop-Buddy_{{VERSION}}_x64-setup.exe` and compare
with the line in `SHA256SUMS.txt`.

<!-- Maintainer: add a short summary of the highlights here before publishing. GitHub's
     generated list of merged pull requests is appended below. -->
