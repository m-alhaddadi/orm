"""Compatibility namespace forwarding to one selected native profile."""
from ._profiles import load as _load

_engine = _load()
globals().update({name: getattr(_engine, name) for name in dir(_engine) if not name.startswith("__")})
