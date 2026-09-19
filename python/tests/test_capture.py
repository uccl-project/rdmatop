import os
from unittest.mock import Mock

import pytest
from rdmatop import _capture


class NativeCapture:
    def __init__(self):
        self.device = b"mlx5_0"
        self.handles = set()
        self.stopped = set()
        self.stop_error = None

    def rdmatop_capture_start(self, interval_us, devices):
        handle = 2**40
        self.handles.add(handle)
        return handle

    def rdmatop_capture_for_each(self, handle, callback, context):
        assert handle in self.handles
        self.stopped.add(handle)
        callback(context, 1_800_000_000_000_000_000, self.device, 2, b"tx_gbps", 12.5)

    def rdmatop_capture_error(self, handle):
        return None

    def rdmatop_capture_stop(self, handle):
        self.stopped.add(handle)
        if self.stop_error is not None:
            raise self.stop_error

    def rdmatop_capture_free(self, handle):
        assert handle in self.stopped
        self.handles.remove(handle)


class InterruptedSamples:
    def append(self, sample):
        raise KeyboardInterrupt


@pytest.fixture
def native_capture(monkeypatch):
    library = NativeCapture()
    monkeypatch.setattr(_capture, "_load_library", Mock(return_value=library))
    return library


def test_capture_replays_native_samples_and_close_is_idempotent(native_capture):
    capture = _capture.Capture(1000, None)
    samples, error = capture.stop()
    assert samples == [(1_800_000_000_000_000_000, "mlx5_0", 2, "tx_gbps", 12.5)]
    assert error is None
    capture.close()
    capture.close()
    assert not native_capture.handles


def test_callback_failure_is_reported_instead_of_silently_losing_samples(
    native_capture,
):
    native_capture.device = None
    capture = _capture.Capture(1000, None)
    with pytest.raises(AttributeError, match="decode"):
        capture.stop()
    capture.close()
    assert not native_capture.handles


def test_callback_preserves_keyboard_interrupt(native_capture, monkeypatch):
    collector = _capture._SampleCollector()
    collector.samples = InterruptedSamples()
    monkeypatch.setattr(_capture, "_SampleCollector", Mock(return_value=collector))
    capture = _capture.Capture(1000, None)
    with pytest.raises(KeyboardInterrupt):
        capture.stop()
    capture.close()
    assert not native_capture.handles


def test_forked_capture_does_not_join_or_free_parent_handle(
    native_capture, monkeypatch
):
    capture = _capture.Capture(1000, None)
    child_pid = os.getpid() + 1
    monkeypatch.setattr(_capture.os, "getpid", Mock(return_value=child_pid))
    assert capture.stop() == ([], None)
    capture.close()
    assert native_capture.handles == {2**40}
    assert not native_capture.stopped


@pytest.mark.parametrize("error", [RuntimeError("stop failed"), KeyboardInterrupt()])
def test_close_frees_handle_when_stopping_raises(native_capture, error):
    native_capture.stop_error = error
    capture = _capture.Capture(1000, None)
    with pytest.raises(type(error)):
        capture.close()
    assert not native_capture.handles
    capture.close()
