# Application icon

`pvd-studio.png` is the canonical 1254 × 1254 RGBA artwork (selected candidate B).
`pvd-studio.ico` contains standard resizes at 16, 24, 32, 48, 64, 128 and 256 pixels;
the image is neither cropped nor redrawn. Both assets retain transparency.

Recreate the ICO with Pillow from the repository root:

```powershell
python -c 'from PIL import Image; Image.open("crates/pvd-desktop/assets/pvd-studio.png").save("crates/pvd-desktop/assets/pvd-studio.ico", format="ICO", sizes=[(s,s) for s in [16,24,32,48,64,128,256]])'
```

Windows MSVC builds use the installed Windows SDK resource compiler and embed
resource `1` in `pvd-desktop.exe`. The main thread loads that resource for the
native window and taskbar icons, with the small icon refreshed when DPI changes.
The PNG and ICO do not need to be distributed beside the executable.

Inspect all seven embedded resource payloads and exercise Windows icon loading
without starting the executable:

```powershell
python tools/diagnostics/verify_windows_icon.py target/release/pvd-desktop.exe
```
