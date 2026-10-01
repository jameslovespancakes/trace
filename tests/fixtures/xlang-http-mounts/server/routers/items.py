"""Fixture (round 2, http mounts): router with one route."""
from fastapi import APIRouter

router = APIRouter()


@router.get("/items/{item_id}")
def read_item(item_id: int):
    return {"id": item_id}
