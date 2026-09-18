import os
import sys
from pathlib import Path


def main():
    executable = Path(__file__).resolve().parent / "bin" / "rdmatop"
    os.execv(str(executable), [str(executable), *sys.argv[1:]])
