# =============================================================================
# LMForge - Windows PowerShell Installer (legacy entry point)
# Kept at the repo root for backwards compatibility with old instructions.
# Delegates to the maintained installer published with each release:
#   irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.ps1 | iex
# =============================================================================
$ErrorActionPreference = "Stop"
irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.ps1 | iex
