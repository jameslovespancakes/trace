import pytest

from app.base import App


@pytest.fixture
def app():
    return App()
