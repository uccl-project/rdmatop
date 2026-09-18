import os
import shutil
import subprocess
import sys
from pathlib import Path

import torch
from setuptools import setup
from torch.utils.cpp_extension import BuildExtension, CppExtension

ROOT = os.path.abspath(os.path.dirname(__file__))
TARGET_DIR = os.path.join(ROOT, "target")
STATICLIB = os.path.join(TARGET_DIR, "release", "librdmatop.a")
KINETO_DIR = os.path.join(ROOT, "kineto")
CAPTURE_HEADER = os.path.join(KINETO_DIR, "rdmatop_capture.h")
SHIM_SOURCE = "kineto/rdmatop_kineto.cpp"
TORCH_DIR = os.path.dirname(torch.__file__)
TORCH_LIB = os.path.join(TORCH_DIR, "lib")
KINETO_INCLUDE = os.path.join(TORCH_DIR, "include", "kineto")
RUST_RUNTIME_LIBS = ["-ldl", "-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm"]


def kineto_capabilities():
    headers = Path(KINETO_INCLUDE)
    activity_types = (headers / "ActivityType.h").read_text()
    trace_activity = (headers / "GenericTraceActivity.h").read_text()
    # Older wheels do not export fmt symbols used by inline Kineto metadata APIs.
    return [
        ("FMT_HEADER_ONLY", "1"),
        ("RDMATOP_NATIVE_COUNTERS", str(int("MTIA_COUNTERS" in activity_types))),
        ("RDMATOP_TYPED_COUNTERS", str(int("addCounterValue(" in trace_activity))),
    ]


class BuildRustThenExt(BuildExtension):
    def run(self):
        if sys.platform != "linux":
            raise SystemExit("rdmatop: only Linux is supported")
        if shutil.which("cargo") is None:
            raise SystemExit("rdmatop: cargo not found on PATH; install Rust first")
        env = {**os.environ, "CARGO_TARGET_DIR": TARGET_DIR}
        subprocess.check_call(
            ["cargo", "build", "--release", "--lib", "--bin", "rdmatop"],
            cwd=ROOT,
            env=env,
        )
        super().run()
        binary_dir = (
            Path(self.get_ext_fullpath("rdmatop._rdmatop_kineto")).parent / "bin"
        )
        binary_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(Path(TARGET_DIR) / "release" / "rdmatop", binary_dir / "rdmatop")

    def get_outputs(self):
        binary = (
            Path(self.get_ext_fullpath("rdmatop._rdmatop_kineto")).parent
            / "bin"
            / "rdmatop"
        )
        return [*super().get_outputs(), str(binary)]


def cargo_version():
    with open(os.path.join(ROOT, "Cargo.toml")) as manifest:
        lines = manifest.read().splitlines()
    for line in lines:
        if line.startswith("version = "):
            return line.split('"')[1]
    raise SystemExit("rdmatop: version not found in Cargo.toml")


setup(
    version=cargo_version(),
    # Kineto's C++ ABI is tied to the PyTorch version used for compilation.
    install_requires=[f"torch=={torch.__version__.split('+')[0]}"],
    ext_modules=[
        CppExtension(
            name="rdmatop._rdmatop_kineto",
            sources=[SHIM_SOURCE],
            include_dirs=[KINETO_INCLUDE, KINETO_DIR],
            depends=[STATICLIB, CAPTURE_HEADER],
            extra_objects=[STATICLIB],
            define_macros=kineto_capabilities(),
            extra_link_args=[
                f"-Wl,-rpath,{TORCH_LIB}",
                "-Wl,--exclude-libs,ALL",
            ]
            + RUST_RUNTIME_LIBS,
            libraries=["torch_cpu"],
        )
    ],
    cmdclass={"build_ext": BuildRustThenExt},
)
