import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

from setuptools import Distribution, setup

# Keep this selection aligned with the build dependency marker in pyproject.toml.
BUILD_KINETO = platform.machine() not in {"armv6l", "armv7l", "armv8l"}
if BUILD_KINETO:
    import torch
    from torch.utils.cpp_extension import BuildExtension, CppExtension
else:
    from setuptools.command.build_ext import build_ext as BuildExtension

ROOT = os.path.abspath(os.path.dirname(__file__))
TARGET_DIR = os.path.join(ROOT, "target")
STATICLIB = os.path.join(TARGET_DIR, "release", "librdmatop.a")
KINETO_DIR = os.path.join(ROOT, "kineto")
CAPTURE_HEADER = os.path.join(KINETO_DIR, "rdmatop_capture.h")
SHIM_SOURCE = "kineto/rdmatop_kineto.cpp"
RUST_RUNTIME_LIBS = ["-ldl", "-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm"]


def kineto_capabilities(include_dir):
    headers = Path(include_dir)
    activity_types = (headers / "ActivityType.h").read_text()
    trace_activity = (headers / "GenericTraceActivity.h").read_text()
    # Older wheels do not export fmt symbols used by inline Kineto metadata APIs.
    return [
        ("FMT_HEADER_ONLY", "1"),
        ("RDMATOP_NATIVE_COUNTERS", str(int("MTIA_COUNTERS" in activity_types))),
        ("RDMATOP_TYPED_COUNTERS", str(int("addCounterValue(" in trace_activity))),
    ]


class NativeDistribution(Distribution):
    def has_ext_modules(self):
        # The Rust executable and shared library are native even without Kineto.
        return True


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
        self.build_kineto()
        binary_dir = (
            Path(self.get_ext_fullpath("rdmatop._rdmatop_kineto")).parent / "bin"
        )
        binary_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(Path(TARGET_DIR) / "release" / "rdmatop", binary_dir / "rdmatop")
        shutil.copy2(
            Path(TARGET_DIR) / "release" / "librdmatop.so",
            binary_dir.parent / "librdmatop.so",
        )

    def build_kineto(self):
        if BUILD_KINETO:
            super().run()

    def get_outputs(self):
        binary = (
            Path(self.get_ext_fullpath("rdmatop._rdmatop_kineto")).parent
            / "bin"
            / "rdmatop"
        )
        return [
            *super().get_outputs(),
            str(binary),
            str(binary.parent.parent / "librdmatop.so"),
        ]


def cargo_version():
    with open(os.path.join(ROOT, "Cargo.toml")) as manifest:
        lines = manifest.read().splitlines()
    for line in lines:
        if line.startswith("version = "):
            return line.split('"')[1]
    raise SystemExit("rdmatop: version not found in Cargo.toml")


def kineto_extensions():
    if not BUILD_KINETO:
        return []
    torch_dir = Path(torch.__file__).parent
    torch_lib = str(torch_dir / "lib")
    kineto_include = str(torch_dir / "include" / "kineto")
    extension = CppExtension(
        name="rdmatop._rdmatop_kineto",
        sources=[SHIM_SOURCE],
        include_dirs=[kineto_include, KINETO_DIR],
        depends=[STATICLIB, CAPTURE_HEADER],
        extra_objects=[STATICLIB],
        define_macros=kineto_capabilities(kineto_include),
        extra_link_args=[
            f"-Wl,-rpath,{torch_lib}",
            "-Wl,--exclude-libs,ALL",
        ]
        + RUST_RUNTIME_LIBS,
        libraries=["torch_cpu"],
    )
    return [extension]


def runtime_dependencies():
    if not BUILD_KINETO:
        return []
    # Kineto's C++ ABI is tied to the PyTorch version used for compilation.
    return [f"torch=={torch.__version__.split('+')[0]}"]


setup(
    version=cargo_version(),
    install_requires=runtime_dependencies(),
    ext_modules=kineto_extensions(),
    distclass=NativeDistribution,
    cmdclass={"build_ext": BuildRustThenExt},
)
