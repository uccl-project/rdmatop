import ctypes
import importlib.util
from functools import wraps

import torch

from rdmatop._trace import convert_legacy_trace

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


def _install_export_compatibility():
    original_export = torch.profiler.profile.export_chrome_trace

    @wraps(original_export)
    def export_with_counters(profiler, path, *args, **kwargs):
        result = original_export(profiler, path, *args, **kwargs)
        convert_legacy_trace(path)
        return result

    torch.profiler.profile.export_chrome_trace = export_with_counters


def enable():
    global _lib
    if _lib is not None:
        return
    lib = _load_shim()
    lib.rdmatop_kineto_register.restype = ctypes.c_int
    lib.rdmatop_kineto_register.argtypes = []
    lib.rdmatop_kineto_register()
    lib.rdmatop_kineto_has_native_counters.restype = ctypes.c_int
    lib.rdmatop_kineto_has_native_counters.argtypes = []
    if not lib.rdmatop_kineto_has_native_counters():
        _install_export_compatibility()
    _lib = lib
