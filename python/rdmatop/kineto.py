import ctypes
import importlib.util

import torch  # noqa: F401

_lib = None


def _shim_path():
    spec = importlib.util.find_spec("rdmatop._rdmatop_kineto")
    if spec is None or spec.origin is None:
        raise ImportError("rdmatop kineto shim is not built for this interpreter")
    return spec.origin


def _load_shim():
    try:
        return ctypes.CDLL(_shim_path())
    except OSError as error:
        raise ImportError(f"cannot load rdmatop kineto shim: {error}") from error


def enable():
    global _lib
    if _lib is not None:
        return
    lib = _load_shim()
    lib.rdmatop_kineto_register.restype = ctypes.c_int
    lib.rdmatop_kineto_register.argtypes = []
    lib.rdmatop_kineto_register()
    _lib = lib
