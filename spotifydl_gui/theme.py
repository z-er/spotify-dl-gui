# spotifydl_gui/theme.py
"""
Qt dark theme + orange-accent styling for spotify-dl GUI.

Keep this file UI-agnostic: no imports of app widgets beyond Qt.
"""

from __future__ import annotations

from PySide6.QtGui import QPalette, QColor
from PySide6.QtWidgets import QApplication


def apply_dark_theme(app: QApplication) -> None:
    """
    Apply a cohesive dark palette and stylesheet across the app.
    """
    # Base colors
    bg         = QColor("#0f131a")  # window background
    bg_alt     = QColor("#141a22")  # panels / buttons
    surface    = QColor("#1a212b")  # inputs
    border     = QColor("#2a2f39")  # outlines
    text       = QColor("#e6eaf2")  # primary text
    text_muted = QColor("#b5bcc9")  # secondary text
    accent     = QColor("#f4a261")  # orange accent
    danger     = QColor("#f7768e")  # error / destructive

    # Qt palette
    pal = QPalette()
    pal.setColor(QPalette.Window, bg)
    pal.setColor(QPalette.WindowText, text)
    pal.setColor(QPalette.Base, surface)
    pal.setColor(QPalette.AlternateBase, bg_alt)
    pal.setColor(QPalette.ToolTipBase, surface)
    pal.setColor(QPalette.ToolTipText, text)
    pal.setColor(QPalette.Text, text)
    pal.setColor(QPalette.Button, bg_alt)
    pal.setColor(QPalette.ButtonText, text)
    pal.setColor(QPalette.BrightText, danger)
    pal.setColor(QPalette.Link, accent)
    pal.setColor(QPalette.Highlight, accent)
    pal.setColor(QPalette.HighlightedText, QColor("#0b0f16"))
    pal.setColor(QPalette.PlaceholderText, text_muted)
    app.setPalette(pal)

    # Checkbox tick icon (inline SVG -> white check)
    check_svg = (
        "url(\"data:image/svg+xml;utf8,"
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 20'>"
        "<path d='M5 10.5 8.5 14 15 6' fill='none' stroke='%23ffffff' stroke-width='2.2' "
        "stroke-linecap='round' stroke-linejoin='round'/>"
        "</svg>\")"
    )

    # Global stylesheet
    app.setStyleSheet(f"""
        /* ====== Base ====== */
        QWidget {{
            background: {bg.name()};
            color: {text.name()};
            font-family: Segoe UI, Inter, -apple-system, Arial;
            font-size: 12.5px;
        }}
        QLabel {{ color: {text.name()}; }}
        .muted {{ color: {text_muted.name()}; }}

        /* ====== Inputs ====== */
        QLineEdit, QTextEdit, QComboBox, QSpinBox, QDoubleSpinBox {{
            background: {surface.name()};
            color: {text.name()};
            border: 1px solid {border.name()};
            border-radius: 10px;
            padding: 8px;
            selection-background-color: {accent.name()};
            selection-color: #0b0f16;
        }}
        QLineEdit:focus, QTextEdit:focus, QComboBox:focus, QSpinBox:focus, QDoubleSpinBox:focus {{
            border-color: {accent.name()};
            outline: 2px solid {accent.name()};
        }}
        QTextEdit {{ padding: 8px; }}

        /* ====== Buttons ====== */
        QPushButton {{
            background: {bg_alt.name()};
            color: {text.name()};
            border: 1px solid {border.name()};
            border-radius: 10px;
            padding: 9px 14px;
        }}
        QPushButton:hover {{ background: {surface.name()}; }}
        QPushButton:pressed {{ background: {border.name()}; }}
        QPushButton:disabled {{
            color: {text_muted.name()};
            border-color: {border.name()};
            background: {bg.name()};
        }}

        /* ====== Checkboxes / Radios ====== */
        QCheckBox, QRadioButton {{ color: {text.name()}; spacing: 8px; }}
        QCheckBox::indicator, QRadioButton::indicator {{ width: 18px; height: 18px; }}
        QCheckBox::indicator {{
            border: 1px solid {border.name()};
            border-radius: 5px;
            background: {surface.name()};
        }}
        QCheckBox::indicator:hover {{ border-color: {accent.name()}; }}
        QCheckBox::indicator:checked {{
            background: {accent.name()};
            border-color: {accent.name()};
            image: {check_svg};
        }}

        /* ====== Combo popup ====== */
        QComboBox::drop-down {{ border: none; width: 24px; }}
        QComboBox QAbstractItemView {{
            background: {bg_alt.name()};
            color: {text.name()};
            border: 1px solid {border.name()};
            selection-background-color: {accent.name()};
            selection-color: #0b0f16;
        }}

        /* ====== Scrollbar ====== */
        QScrollBar:vertical {{
            background: {bg.name()};
            width: 12px;
            margin: 2px;
        }}
        QScrollBar::handle:vertical {{
            background: {surface.name()};
            border: 1px solid {border.name()};
            border-radius: 6px;
            min-height: 24px;
        }}
        QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {{ height: 0px; }}

        /* ====== Tooltips ====== */
        QToolTip {{
            background: {surface.name()};
            color: {text.name()};
            border: 1px solid {border.name()};
            padding: 6px 8px;
            border-radius: 8px;
        }}

        /* ====== ProgressBar (queue rows) ====== */
        QProgressBar {{
            border: 1px solid {border.name()};
            border-radius: 8px;
            text-align: center;
            background: {surface.name()};
            height: 16px;
        }}
        QProgressBar::chunk {{
            background-color: {accent.name()};
            border-radius: 8px;
        }}

        /* ====== Decorative line ====== */
        QFrame[frameShape="4"] {{  /* HLine */
            color: {border.name()};
            background-color: {border.name()};
            max-height: 1px;
        }}
    """)
