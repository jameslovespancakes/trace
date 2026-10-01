"""Fixture (P5, openapi): the app mounts the items router under a configured prefix."""
from fastapi import FastAPI

from server import items
from server.settings import API_PREFIX

app = FastAPI()
app.include_router(items.router, prefix=API_PREFIX)
