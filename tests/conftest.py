"""Put the repository root and tools/ on sys.path for the whole suite.

The corpus half imports `fadden` and `build_release_archives`, neither of
which is an installed distribution: the corpus ships archives, not a wheel.
Running through the pytest console script gives no implicit CWD entry, so
without this the corpus tests fail to import while the radar tests pass.
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
for entry in (ROOT, ROOT / "tools"):
    text = str(entry)
    if text not in sys.path:
        sys.path.insert(0, text)
