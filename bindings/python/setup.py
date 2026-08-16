"""Build script: compiles the PyO3 `_slimdb` extension with cargo and packages
it as the `slimdb` Python package.

    pip install .          # from this directory
"""

import glob
import os
import shutil
import subprocess

from setuptools import Extension, setup
from setuptools.command.build_ext import build_ext

HERE = os.path.dirname(os.path.abspath(__file__))


class BuildRustExt(build_ext):
    """Build the Rust extension module and place it where setuptools expects."""

    def build_extension(self, ext):
        subprocess.run(
            ["cargo", "build", "--release", "--manifest-path", os.path.join(HERE, "Cargo.toml")],
            check=True,
            env=dict(os.environ, PYO3_PYTHON="python3"),
        )
        target = os.path.join(HERE, "..", "..", "target", "release")
        # Artifact name depends on the platform (cdylib of the `_slimdb` crate).
        patterns = ["lib_slimdb*.so", "lib_slimdb*.dylib", "_slimdb*.pyd", "_slimdb*.dll"]
        libs = [f for pat in patterns for f in glob.glob(os.path.join(target, pat))]
        if not libs:
            raise RuntimeError(f"cargo did not produce a slimdb extension in {target}")
        src = max(libs, key=os.path.getmtime)
        dest_dir = os.path.dirname(self.get_ext_fullpath(ext.name))
        os.makedirs(dest_dir, exist_ok=True)
        dest = os.path.join(dest_dir, os.path.basename(self.get_ext_filename(ext.name)))
        shutil.copy(src, dest)
        self._copied = dest

    def get_ext_filename(self, fullname):
        # The extension is already built by cargo; importable suffix per OS.
        if fullname == "slimdb._slimdb":
            suffix = ".pyd" if os.name == "nt" else ".so"
            return os.path.join(*fullname.split(".")) + suffix
        return super().get_ext_filename(fullname)


setup(
    name="slimdb",
    version="0.1.0",
    description="The tiny embedded database for local apps. No server. No configuration. No SQL.",
    long_description=open(os.path.join(HERE, "README.md"), encoding="utf-8").read(),
    long_description_content_type="text/markdown",
    packages=["slimdb"],
    ext_modules=[Extension("slimdb._slimdb", sources=[])],
    cmdclass={"build_ext": BuildRustExt},
    python_requires=">=3.8",
)
