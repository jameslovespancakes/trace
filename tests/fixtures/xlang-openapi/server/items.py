"""Fixture (P5, openapi): FastAPI router mounted under a non-literal prefix."""
from fastapi import APIRouter

router = APIRouter(prefix="/items")


@router.get("/{item_id}")
def read_item(item_id: int):  # unique handler of operation read_item -> inferred
    return {"id": item_id}


@router.post("/")
def create_item():  # also registered in server/legacy.py -> possible
    return {}
