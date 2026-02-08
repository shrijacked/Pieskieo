# Installers (to re-add later)

This repo currently ships without installers. To build one:
- A cross-platform installer script should download the prebuilt ZIP for the host platform (from GitHub releases).
- Place binaries into `/usr/local/bin` on Unix (requires sudo) or `%ProgramData%\Pieskieo\bin` on Windows.
- Optionally register a systemd service for the server on Linux.

We removed prior installers to avoid PATH/logging issues. Reintroduce them only after the CLI/server interfaces stabilize.
