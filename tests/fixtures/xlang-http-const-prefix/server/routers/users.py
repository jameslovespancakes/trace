"""Fixture (round 2, http constant prefix): mounted under a prefix bound twice (unknown)."""
from fastapi import APIRouter

router = APIRouter(prefix="/users")


@router.get("/{user_id}")
def read_user(user_id: int):
    return {"id": user_id}
