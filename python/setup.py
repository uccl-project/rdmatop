import os
import shutil
import subprocess

import torch
from setuptools import setup
from torch.utils.cpp_extension import BuildExtension, CppExtension

HERE = os.path.abspath(os.path.dirname(__file__))
ROOT = os.path.dirname(HERE)
TARGET_DIR = os.path.join(ROOT, "target")
STATICLIB = os.path.join(TARGET_DIR, "release", "librdmatop.a")
KINETO_DIR = os.path.join(ROOT, "kineto")
CAPTURE_HEADER = os.path.join(KINETO_DIR, "rdmatop_capture.h")
SHIM_SOURCE = os.path.relpath(os.path.join(KINETO_DIR, "rdmatop_kineto.cpp"), HERE)
TORCH_DIR = os.path.dirname(torch.__file__)
TORCH_LIB = os.path.join(TORCH_DIR, "lib")
KINETO_INCLUDE = os.path.join(TORCH_DIR, "include", "kineto")
RUST_RUNTIME_LIBS = ["-ldl", "-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm"]


class BuildRustThenExt(BuildExtension):
    def run(self):
        if shutil.which("cargo") is None:
            raise SystemExit("rdmatop: cargo not found on PATH; install Rust first")
        env = {**os.environ, "CARGO_TARGET_DIR": TARGET_DIR}
        subprocess.check_call(["cargo", "build", "--release", "--lib"], cwd=ROOT, env=env)
        super().run()


def cargo_version():
    with open(os.path.join(ROOT, "Cargo.toml")) as manifest:
        lines = manifest.read().splitlines()
    for line in lines:
        if line.startswith("version = "):
            return line.split('"')[1]
    raise SystemExit("rdmatop: version not found in Cargo.toml")


setup(
    version=cargo_version(),
    ext_modules=[
        CppExtension(
            name="rdmatop._rdmatop_kineto",
            sources=[SHIM_SOURCE],
            include_dirs=[KINETO_INCLUDE, KINETO_DIR],
            depends=[STATICLIB, CAPTURE_HEADER],
            extra_objects=[STATICLIB],
            extra_compile_args=["-std=c++17"],
            extra_link_args=[f"-Wl,-rpath,{TORCH_LIB}", "-Wl,--exclude-libs,ALL"]
            + RUST_RUNTIME_LIBS,
            libraries=["torch_cpu"],
        )
    ],
    cmdclass={"build_ext": BuildRustThenExt},
)
