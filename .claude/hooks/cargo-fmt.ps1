# PostToolUse(Edit|Write) hook: run cargo fmt after a Rust source edit. Gated on
# .rs so markdown/json/toml edits don't trigger a format pass. Always exits 0.
$raw = [Console]::In.ReadToEnd()
try { $j = $raw | ConvertFrom-Json } catch { exit 0 }
$path = $j.tool_input.file_path
if ($path -notmatch '\.rs$') { exit 0 }
if ($env:CLAUDE_PROJECT_DIR) { Set-Location $env:CLAUDE_PROJECT_DIR }
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
try { cargo fmt 2>$null } catch {}
exit 0
