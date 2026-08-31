"""Keep the test suite away from the user's production task database."""

from __future__ import annotations

import os
import shutil
import tempfile
from pathlib import Path


TEST_DATA_ROOT = Path(tempfile.mkdtemp(prefix="local-desensitization-tests-"))
os.environ["LOCAL_DESENSITIZATION_DATA_DIR"] = str(TEST_DATA_ROOT)


def pytest_sessionfinish(session, exitstatus) -> None:  # type: ignore[no-untyped-def]
    shutil.rmtree(TEST_DATA_ROOT, ignore_errors=True)
