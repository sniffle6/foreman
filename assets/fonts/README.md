# Terminal fonts (Hack)

Pinned Hack release used for real bold/italic terminal faces.

| Field | Value |
|-------|--------|
| Upstream | [source-foundry/Hack](https://github.com/source-foundry/Hack) |
| Version | **v3.003** |
| Source URL | https://github.com/source-foundry/Hack/releases/download/v3.003/Hack-v3.003-ttf.zip |
| Release zip SHA-256 | `0C2604631B1F055041C68A0E09AE4801ACAB6C5072BA2DB6A822F53C3F8290AC` |
| License | MIT + Bitstream Vera (see `LICENSE-Hack.md`) |

## Face files

| File | Role | SHA-256 |
|------|------|---------|
| `Hack-Regular.ttf` | regular | `15F55CC0C85A2988D2B4B3A8CDB5D77FDFBAF319E1BB5309D725DB9818FB7125` |
| `Hack-Bold.ttf` | bold | `5BBF531EFF7F8A0C2559C9A0656718E2828A012A9B1F60B5F54006D59A4DE8D4` |
| `Hack-Italic.ttf` | italic | `096FB67A2B85F3C866E9CB3E965B27C2C10B977315F4D3D7F095674BE35091C1` |
| `Hack-BoldItalic.ttf` | bold+italic | `64F74A079700B7DFE128551A1E28875D5BA980971E55F5E0F0596E37BDC6A6BC` |

All four faces come from the same zip. Do not mix these with egui's embedded
regular face for terminal paint — terminal text uses the named families
registered by `src/terminal_font.rs`.

## Verify

```powershell
Get-FileHash assets/fonts/Hack-*.ttf -Algorithm SHA256
```
