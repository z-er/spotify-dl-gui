# spotifydl_gui/__init__.py
"""
spotify-dl GUI — package init
"""

from __future__ import annotations

__all__ = [
    "__version__",
    "main",
]

__version__ = "0.6"

# Re-export the CLI entry for convenience: `python -m spotifydl_gui`
from .main import main  # noqa: E402
