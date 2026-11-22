"""CLI proxy package."""

from importlib.metadata import PackageNotFoundError, version

try:
    __version__ = version("crawlberg-cli")
except PackageNotFoundError:
    __version__ = "0+unknown"
