"""Fixture (round 2, http constant prefix): the mount prefix is a settings attribute, not a
literal at the include site; the bridge stage resolves it through the import."""
from fastapi import FastAPI

from .core.config import other, settings
from .routers import items, users

app = FastAPI()
app.include_router(items.router, prefix=settings.API_V1_STR)
app.include_router(users.router, prefix=other.PREFIX)
