"""Fixture (P5, openapi): a second app registering the same operation and handler name."""
from fastapi import FastAPI

legacy_app = FastAPI()


@legacy_app.post("/items")
def create_item():
    return {"legacy": True}
