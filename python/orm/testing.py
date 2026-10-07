"""pytest plugin of the ORM: add ``pytest_plugins = ["orm.testing"]`` to a ``conftest.py``.

The ``n_plus_one`` fixture fails a test that sends an N+1 (see :mod:`orm.debug`)::

    async def test_list_view(n_plus_one):
        await render_customers()   # fails if one query shape runs more than 5 times

``n_plus_one.threshold = 10`` in the test changes the threshold of that test.
"""

from __future__ import annotations

from collections.abc import Generator

import pytest

from .debug import Report, _scope


@pytest.fixture
def n_plus_one() -> Generator[Report]:
    """Counts the queries of the test by statement shape; fails the test at teardown
    when one shape ran more than ``threshold`` (default 5) times."""
    report = Report(5)
    token = _scope.set(report)
    try:
        yield report
    finally:
        _scope.reset(token)
    if report.repeated:
        pytest.fail(f"N+1 queries in the test:\n{report.message()}", pytrace=False)
