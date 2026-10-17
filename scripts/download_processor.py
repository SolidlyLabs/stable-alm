"""Wrapper kept for the backtest workflow; the code lives in alm.download.processor."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.download.processor import *  # noqa: E402,F401,F403
from alm.download.processor import main  # noqa: E402

if __name__ == "__main__":
    main()
