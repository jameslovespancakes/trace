"""Fixture (round 2, http mounts): the same router included twice under the same prefix
(two mount chains, one distinct prefix) must yield one route, not two ambiguous copies."""
from fastapi import FastAPI

from .routers import items

app = FastAPI()
app.include_router(items.router, prefix="/api")
app.include_router(items.router, prefix="/api")
