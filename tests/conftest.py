"""Put the repository root on sys.path for the whole suite.

The corpus half imports `fadden`, which is not an installed distribution.
Running through the pytest console script gives no implicit CWD entry, so
without this the corpus tests fail to import while the radar tests pass.
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
root_text = str(ROOT)
if root_text not in sys.path:
    sys.path.insert(0, root_text)
