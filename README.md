# RemoteDesk

Remote desktop for Windows. Machines connect peer-to-peer over [iroh](https://github.com/n0-computer/iroh), so no server of your own is needed. It ships as a single executable with no installer.

## Features

- Permanent 10-digit ID tied to the machine, with online status for saved contacts
- Mouse and keyboard control, including the Windows key and Alt+Tab
- Copy text and files between machines, and chat during a session
- The remote side can accept a connection and choose permissions, or allow access by password
- Access log and temporary lockout after repeated wrong passwords
- Automatic updates with signature verification

## Installation

Download `RemoteDesk.exe` from [Releases](https://github.com/GoneSeeder/RemoteDesk/releases/latest). The program requires administrator rights. The executable is not code-signed yet, so Windows SmartScreen may show a warning.

## Build

```bash
cargo build --release
```

To publish a release, push a tag `vX.Y.Z` that matches `version` in `Cargo.toml`. GitHub Actions builds, signs and publishes it (requires the `REMOTEDESK_SIGNING_KEY` secret).

## Limitations

Windows only. Ctrl+Alt+Del cannot be sent, there is no audio, and it is not recommended for critical servers.
