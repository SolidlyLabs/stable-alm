"""Where the backtests and the reference executor keep their state.

HOME  config, logs, data, secrets: ALM_HOME if set, else the repo root.
alm/abi is read relative to the modules themselves.
"""
import os

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOME = os.path.abspath(os.environ.get("ALM_HOME") or REPO)
CONFIG = os.path.join(HOME, "config")
LOGS = os.path.join(HOME, "logs")
DATA = os.path.join(HOME, "data")
SECRETS = os.environ.get("ALM_SECRETS_DIR") or os.path.join(HOME, "secrets")
