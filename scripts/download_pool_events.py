"""Wrapper kept for the backtest workflow; the code lives in alm.download.pool_events."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.download.pool_events import *  # noqa: E402,F401,F403
from alm.download.pool_events import main  # noqa: E402

if __name__ == "__main__":
    main()
