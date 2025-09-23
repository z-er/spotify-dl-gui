# -*- mode: python ; coding: utf-8 -*-
import pathlib

if '__file__' in globals():
    project_root = pathlib.Path(__file__).resolve().parent
else:
    project_root = pathlib.Path.cwd()

datas = [
    (str(project_root / 'README.md'), '.'),
    (str(project_root / 'spotify-dl-gui.ico'), '.'),
]

spotify_dl_binary = project_root / 'spotify-dl.exe'
if spotify_dl_binary.exists():
    datas.append((str(spotify_dl_binary), '.'))

additional_binaries = []

a = Analysis(
    ['run_app.py'],
    pathex=[],
    binaries=additional_binaries,
    datas=datas,
    hiddenimports=[],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
    optimize=0,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name='spotify-dl-gui',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=[str(project_root / 'spotify-dl-gui.ico')],
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=True,
    upx_exclude=[],
    name='spotify-dl-gui',
)
