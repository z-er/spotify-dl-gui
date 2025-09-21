# spotifydl_gui/organizer.py
"""
Organizer & metadata utilities for spotify-dl GUI.

Responsibilities:
- Discover freshly downloaded audio files
- Read tags (artist/album/title/track/disc/year) via mutagen
- Compute destination subfolders from a user template
- Move files into organized structure
- Handle duplicates (keep larger or skip/delete smaller)
- Extract embedded cover art when missing
- Flag "suspect" files by size and/or duration threshold (integrity checks)

Public entry point:
    organize_new_files(dest_root, pre_files, run_started_at, settings)
returns: (outputs, suspects, stats)
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import shutil
import re
from typing import Iterable, Tuple, List, Dict, Set, Optional

from mutagen import File as MutagenFile
from mutagen.id3 import ID3
from mutagen.mp4 import MP4, MP4Cover
from mutagen.flac import FLAC, Picture as FLACPicture

# ----------------------------
# Constants / helpers
# ----------------------------
AUDIO_EXTS = {".flac", ".mp3", ".m4a", ".mp4", ".opus", ".ogg", ".wav"}

_BAD_FS_CHARS = '<>:"/\\|?*'
_SANITIZE_RE = re.compile(r"[^\w\s\-.&'()]+", re.UNICODE)


def sanitize_component(s: str) -> str:
    """
    Make a filesystem-safe path component; keep it readable.
    """
    if not s:
        return "_"
    s = "".join("_" if c in _BAD_FS_CHARS else c for c in s)
    # collapse runs of illegal chars to single underscore, strip ends & dots
    s = _SANITIZE_RE.sub("_", s).strip().strip(".")
    s = re.sub(r"_+", "_", s)
    return s or "_"


def list_audio_files(root: Path) -> Set[str]:
    files: Set[str] = set()
    if not root.exists():
        return files
    for p in root.rglob("*"):
        if p.is_file() and p.suffix.lower() in AUDIO_EXTS:
            try:
                files.add(str(p.resolve()))
            except Exception:
                files.add(str(p))
    return files


def _replace_file(src: Path, dst: Path) -> None:
    """
    Replace dst atomically-ish with src (best effort on Windows).
    """
    tmp = dst.with_suffix(dst.suffix + ".tmp.replace")
    try:
        if dst.exists():
            try:
                dst.replace(tmp)
            except Exception:
                try:
                    dst.unlink()
                except Exception:
                    tmp = None
        src.replace(dst)
    finally:
        if tmp and tmp.exists():
            try:
                tmp.unlink()
            except Exception:
                pass


# ----------------------------
# Tag reading
# ----------------------------
def read_tags(path: Path) -> Dict:
    """
    Read common tags using mutagen, with safe fallbacks.
    """
    tags = {
        "artist": "",
        "album": "",
        "title": path.stem,
        "track": 0,
        "disc": 1,
        "year": 0,
        "ext": path.suffix,
        "filename": path.name,
    }
    ext = path.suffix.lower()
    try:
        if ext == ".mp3":
            id3 = ID3(str(path))
            if id3.get("TALB"):
                tags["album"] = str(id3.get("TALB").text[0])
            for t in ("TPE1", "TPE2"):
                fr = id3.get(t)
                if fr and not tags["artist"]:
                    tags["artist"] = str(fr.text[0])
            if id3.get("TIT2"):
                tags["title"] = str(id3.get("TIT2").text[0])
            if id3.get("TRCK"):
                try:
                    tags["track"] = int(str(id3.get("TRCK").text[0]).split("/")[0])
                except Exception:
                    pass
            if id3.get("TPOS"):
                try:
                    tags["disc"] = int(str(id3.get("TPOS").text[0]).split("/")[0])
                except Exception:
                    pass
            if id3.get("TDRC"):
                try:
                    tags["year"] = int(str(id3.get("TDRC").text[0])[:4])
                except Exception:
                    pass

        elif ext in {".m4a", ".mp4"}:
            mp = MP4(str(path))

            def _get(k):
                v = mp.tags.get(k)
                return v[0] if isinstance(v, list) and v else (v if v else "")

            alb = _get("\xa9alb")
            art = _get("\xa9ART") or _get("aART")
            ttl = _get("\xa9nam")
            trk = mp.tags.get("trkn")
            dsk = mp.tags.get("disk")
            day = _get("\xa9day")

            tags["album"] = alb or ""
            tags["artist"] = art or ""
            tags["title"] = ttl or tags["title"]
            if trk and trk[0] and trk[0][0]:
                tags["track"] = int(trk[0][0])
            if dsk and dsk[0] and dsk[0][0]:
                tags["disc"] = int(dsk[0][0])
            if day:
                try:
                    tags["year"] = int(str(day)[:4])
                except Exception:
                    pass

        elif ext == ".flac":
            fl = FLAC(str(path))
            tags["album"] = (fl.get("album", [""])[0] or "")
            tags["artist"] = (fl.get("artist", [""])[0] or fl.get("albumartist", [""])[0] if fl else "")
            tags["title"] = (fl.get("title", [tags["title"]])[0])
            try:
                tags["track"] = int((fl.get("tracknumber", ["0"])[0]).split("/")[0])
            except Exception:
                pass
            try:
                tags["disc"] = int((fl.get("discnumber", ["1"])[0]).split("/")[0])
            except Exception:
                pass
            try:
                tags["year"] = int((fl.get("date", ["0"])[0])[:4])
            except Exception:
                pass

        else:
            mf = MutagenFile(str(path), easy=True)
            if mf and mf.tags:
                def _get(key):
                    v = mf.tags.get(key)
                    return v[0] if isinstance(v, list) and v else (v if v else "")

                tags["album"] = _get("album") or ""
                tags["artist"] = _get("artist") or _get("albumartist") or ""
                tags["title"] = _get("title") or tags["title"]
                try:
                    tags["track"] = int(str(_get("tracknumber")).split("/")[0])
                except Exception:
                    pass
                try:
                    tags["disc"] = int(str(_get("discnumber")).split("/")[0])
                except Exception:
                    pass
                try:
                    tags["year"] = int(str(_get("date"))[:4])
                except Exception:
                    pass
    except Exception:
        pass

    if not tags["album"]:
        tags["album"] = "Unknown Album"
    if not tags["artist"]:
        tags["artist"] = "Unknown Artist"
    return tags


def audio_duration_seconds(path: Path) -> Optional[float]:
    """
    Best-effort duration using mutagen.info.length (may be None).
    """
    try:
        mf = MutagenFile(str(path))
        if mf and mf.info and getattr(mf.info, "length", None):
            return float(mf.info.length)
    except Exception:
        pass
    return None


# ----------------------------
# Cover extraction
# ----------------------------
def maybe_extract_cover(audio_path: Path, album_dir: Path, enable: bool) -> None:
    """
    If enabled and album_dir lacks cover.xxx, try to extract the first embedded image.
    """
    if not enable:
        return
    for name in ("cover.jpg", "cover.png", "folder.jpg", "folder.png"):
        if (album_dir / name).exists():
            return
    try:
        ext = audio_path.suffix.lower()
        data = None
        is_png = False
        if ext == ".mp3":
            id3 = ID3(str(audio_path))
            apics = id3.getall("APIC")
            if apics:
                data = apics[0].data
                is_png = apics[0].mime == "image/png" or (data[:8] == b"\x89PNG\r\n\x1a\n")
        elif ext in {".m4a", ".mp4"}:
            mp = MP4(str(audio_path))
            covr = mp.tags.get("covr")
            if covr:
                pic = covr[0]
                if isinstance(pic, MP4Cover):
                    data = bytes(pic)
                    is_png = (pic.imageformat == MP4Cover.FORMAT_PNG) or (data[:8] == b"\x89PNG\r\n\x1a\n")
        elif ext == ".flac":
            fl = FLAC(str(audio_path))
            if fl.pictures:
                pic: FLACPicture = fl.pictures[0]
                data = pic.data
                is_png = pic.mime == "image/png" or (data[:8] == b"\x89PNG\r\n\x1a\n")
        if not data:
            return
        fname = "cover.png" if is_png else "cover.jpg"
        with open(album_dir / fname, "wb") as f:
            f.write(data)
    except Exception:
        pass


# ----------------------------
# Organizer core
# ----------------------------
@dataclass
class OrganizerConfig:
    organize_enabled: bool = True
    template: str = "{artist}/{album}"
    dup_resolve_keep_larger: bool = True
    dup_delete_smaller: bool = False
    cover_extract: bool = True
    integrity_flag: bool = True
    integrity_min_mb: float = 1.0
    integrity_duration_flag: bool = False
    integrity_min_seconds: int = 10


def config_from_settings(settings) -> OrganizerConfig:
    """
    Build an OrganizerConfig from QSettings-like object.
    """
    def _b(key: str, default: bool) -> bool:
        return str(settings.value(key, "true" if default else "false")).lower() == "true"

    def _f(key: str, default: float) -> float:
        try:
            return float(settings.value(key, default))
        except Exception:
            return default

    def _i(key: str, default: int) -> int:
        try:
            return int(settings.value(key, default))
        except Exception:
            return default

    return OrganizerConfig(
        organize_enabled=_b("organize_enabled", True),
        template=(settings.value("template", "{artist}/{album}") or "{artist}/{album}").strip(),
        dup_resolve_keep_larger=_b("dup_resolve", True),
        dup_delete_smaller=_b("dup_delete_smaller", False),
        cover_extract=_b("cover_extract", True),
        integrity_flag=_b("integrity_flag", True),
        integrity_min_mb=_f("integrity_min_mb", 1.0),
        integrity_duration_flag=_b("integrity_duration_flag", False),
        integrity_min_seconds=_i("integrity_min_seconds", 10),
    )


def compute_subfolder_from_template(path: Path, template: str) -> Path:
    """
    Compute a sanitized subfolder for a file using the folder template and its tags.
    Supports format like {artist}/{album} and {track:02d}.
    """
    tags = read_tags(path)

    class FmtDict(dict):
        def __missing__(self, k):
            return ""

    # Python-style format with width: {track:02d}
    try:
        sub = template.format_map(FmtDict(tags))
    except Exception:
        sub = template

    parts = [sanitize_component(p) for p in re.split(r"[\\/]+", sub) if p]
    return Path(*parts) if parts else Path()


def _record_output(outputs: List[Dict], stats: Dict[str, int], tags: Dict, final_path: Path, inc_key: str) -> None:
    try:
        size = final_path.stat().st_size
    except Exception:
        size = -1
    outputs.append({
        "artist": tags.get("artist") or "",
        "title": tags.get("title") or final_path.stem,
        "album": tags.get("album") or "",
        "dest": str(final_path),
        "size": int(size),
    })
    stats[inc_key] = stats.get(inc_key, 0) + 1


def _maybe_flag_suspect(suspects: List[Dict], cfg: OrganizerConfig, final_path: Path, tags: Dict) -> None:
    if not cfg.integrity_flag:
        return
    try:
        size_bytes = final_path.stat().st_size
    except Exception:
        size_bytes = -1
    size_mb = (size_bytes / (1024 * 1024)) if size_bytes >= 0 else -1

    reasons = []
    if size_mb >= 0 and size_mb < cfg.integrity_min_mb:
        reasons.append(f"size {size_mb:.2f} MB < {cfg.integrity_min_mb:.2f} MB")

    dur = None
    if cfg.integrity_duration_flag:
        dur = audio_duration_seconds(final_path)
        if dur is not None and dur < float(cfg.integrity_min_seconds):
            reasons.append(f"duration {dur:.1f}s < {cfg.integrity_min_seconds}s")

    if reasons:
        suspects.append({
            "artist": tags.get("artist", ""),
            "title": tags.get("title", ""),
            "album": tags.get("album", ""),
            "dest": str(final_path),
            "size": int(size_bytes if size_bytes >= 0 else 0),
            "duration": (None if dur is None else float(dur)),
            "reason": "; ".join(reasons),
        })


def _move_or_handle_duplicate(src: Path, target: Path, album_dir: Path,
                              cfg: OrganizerConfig, outputs: List[Dict], suspects: List[Dict], stats: Dict[str, int]) -> None:
    """
    Move file to target with duplicate strategy. Records outputs/stats and extracts cover/suspects.
    """
    # Read once for logging & integrity
    tags = read_tags(src if not target.exists() else target)

    if target.exists():
        if cfg.dup_resolve_keep_larger:
            try:
                src_size = src.stat().st_size
                dst_size = target.stat().st_size
            except Exception:
                src_size, dst_size = -1, -1

            if src_size > dst_size:
                temp_in_album = album_dir / (src.name + ".tmp.incoming")
                try:
                    try:
                        shutil.move(str(src), str(temp_in_album))
                    except Exception:
                        shutil.copy2(str(src), str(temp_in_album))
                        src.unlink(missing_ok=True)
                    _replace_file(temp_in_album, target)
                    tags2 = read_tags(target)
                    _record_output(outputs, stats, tags2, target, "replaced")
                    maybe_extract_cover(target, album_dir, cfg.cover_extract)
                    _maybe_flag_suspect(suspects, cfg, target, tags2)
                finally:
                    if 'temp_in_album' in locals() and temp_in_album.exists():
                        try:
                            temp_in_album.unlink()
                        except Exception:
                            pass
            else:
                if cfg.dup_delete_smaller:
                    src.unlink(missing_ok=True)
                    stats["deleted"] = stats.get("deleted", 0) + 1
                else:
                    stats["skipped"] = stats.get("skipped", 0) + 1
        else:
            # Keep both by renaming
            stem, ext = target.stem, target.suffix
            n = 1
            cand = album_dir / f"{stem} ({n}){ext}"
            while cand.exists():
                n += 1
                cand = album_dir / f"{stem} ({n}){ext}"
            try:
                shutil.move(str(src), str(cand))
            except Exception:
                try:
                    shutil.copy2(str(src), str(cand))
                    src.unlink(missing_ok=True)
                except Exception:
                    return
            tags2 = read_tags(cand)
            _record_output(outputs, stats, tags2, cand, "moved")
            maybe_extract_cover(cand, album_dir, cfg.cover_extract)
            _maybe_flag_suspect(suspects, cfg, cand, tags2)
        return

    # Normal move
    try:
        shutil.move(str(src), str(target))
    except Exception:
        try:
            shutil.copy2(str(src), str(target))
            src.unlink(missing_ok=True)
        except Exception:
            return

    tags = read_tags(target)
    _record_output(outputs, stats, tags, target, "moved")
    maybe_extract_cover(target, album_dir, cfg.cover_extract)
    _maybe_flag_suspect(suspects, cfg, target, tags)


def organize_new_files(dest_root: str,
                       pre_files: Optional[Set[str]],
                       run_started_at: Optional[float],
                       settings) -> Tuple[List[Dict], List[Dict], Dict[str, int]]:
    """
    Organize files that appeared since 'pre_files' snapshot and/or after 'run_started_at'.

    Parameters
    ----------
    dest_root : str
        Destination root folder where spotify-dl wrote files.
    pre_files : set[str] | None
        Snapshot of audio file paths before the run. If None, only mtime cutoff is used.
    run_started_at : float | None
        Timestamp when the run started (seconds). Files modified on/after this are considered.
    settings : QSettings-like
        For reading organizer and integrity options.

    Returns
    -------
    outputs : list[dict]
        Records of landed/replaced files: {artist, title, album, dest, size}
    suspects : list[dict]
        Flagged files below thresholds: {artist, title, album, dest, size, duration, reason}
    stats : dict
        Counters: moved, replaced, deleted, skipped
    """
    cfg = config_from_settings(settings)
    outputs: List[Dict] = []
    suspects: List[Dict] = []
    stats: Dict[str, int] = {"moved": 0, "replaced": 0, "deleted": 0, "skipped": 0}

    if not dest_root:
        return outputs, suspects, stats

    root = Path(dest_root)
    root.mkdir(parents=True, exist_ok=True)

    current = list_audio_files(root)
    pre = pre_files or set()
    new_candidates = {Path(p) for p in (current - pre)}

    if run_started_at:
        cutoff = run_started_at
        for p_str in current:
            p = Path(p_str)
            try:
                if p.stat().st_mtime >= cutoff:
                    new_candidates.add(p)
            except Exception:
                pass

    # No organizer? Still integrity-check any new files, but do not move
    if not cfg.organize_enabled:
        for p in new_candidates:
            try:
                tags = read_tags(p)
                _record_output(outputs, stats, tags, p, "moved")  # treat as landed
                _maybe_flag_suspect(suspects, cfg, p, tags)
            except Exception:
                pass
        return outputs, suspects, stats

    # Organizer enabled: compute album/subfolder destination per file
    for p in new_candidates:
        try:
            subfolder = compute_subfolder_from_template(p, cfg.template)
            if not subfolder or str(subfolder).strip("/\\") == "":
                subfolder = Path(sanitize_component(read_tags(p)["album"]))
            album_dir = root / subfolder
            try:
                # prevent spiraling if already in album_dir
                if album_dir.resolve() in p.resolve().parents:
                    # Already inside the right album folder: still integrity-check and maybe extract cover
                    tags = read_tags(p)
                    _record_output(outputs, stats, tags, p, "moved")
                    maybe_extract_cover(p, album_dir, cfg.cover_extract)
                    _maybe_flag_suspect(suspects, cfg, p, tags)
                    continue
            except Exception:
                pass

            album_dir.mkdir(parents=True, exist_ok=True)
            target = album_dir / p.name
            _move_or_handle_duplicate(p, target, album_dir, cfg, outputs, suspects, stats)
        except Exception:
            # Best-effort: ignore single-file failures
            pass

    return outputs, suspects, stats

    # --- NEW: full-library reorganization ---------------------------------------
def reorganize_library(dest_root: str, settings) -> tuple[list[dict], list[dict], dict]:
    """
    Reorganize *all* audio files under dest_root using current settings.
    Applies folder template, duplicate policy, cover extraction, and integrity checks.

    Returns:
        outputs: list of moved/replaced records (artist,title,album,dest,size)
        suspects: list of flagged items (size/duration thresholds)
        stats: dict with counters {moved,replaced,deleted,skipped}
    """
    cfg = config_from_settings(settings)
    outputs: list[dict] = []
    suspects: list[dict] = []
    stats: dict[str, int] = {"moved": 0, "replaced": 0, "deleted": 0, "skipped": 0}

    if not dest_root or not cfg.organize_enabled:
        # If organizer disabled, still integrity-check files
        for p_str in sorted(list_audio_files(Path(dest_root))):
            p = Path(p_str)
            try:
                t = read_tags(p)
                _record_output(outputs, stats, t, p, "moved")
                _maybe_flag_suspect(suspects, cfg, p, t)
            except Exception:
                pass
        return outputs, suspects, stats

    root = Path(dest_root)
    files = [Path(p) for p in sorted(list_audio_files(root))]

    for p in files:
        try:
            subfolder = compute_subfolder_from_template(p, cfg.template)
            if not subfolder or str(subfolder).strip("/\\") == "":
                subfolder = Path(sanitize_component(read_tags(p)["album"]))
            album_dir = root / subfolder

            try:
                # If already inside target folder, still do integrity + cover
                if album_dir.resolve() in p.resolve().parents:
                    tags = read_tags(p)
                    _record_output(outputs, stats, tags, p, "moved")
                    maybe_extract_cover(p, album_dir, cfg.cover_extract)
                    _maybe_flag_suspect(suspects, cfg, p, tags)
                    continue
            except Exception:
                pass

            album_dir.mkdir(parents=True, exist_ok=True)
            target = album_dir / p.name
            _move_or_handle_duplicate(p, target, album_dir, cfg, outputs, suspects, stats)
        except Exception:
            # Skip on error; keep going
            pass

    return outputs, suspects, stats
