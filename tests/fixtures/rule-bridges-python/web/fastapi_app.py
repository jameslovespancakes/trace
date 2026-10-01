"""Fixture (bridges gate, fastapi family): a verb decorator and a mounted router."""
from fastapi import APIRouter, FastAPI

app = FastAPI()
router = APIRouter()


@app.get("/fapi/items/{item_id}")
def read_item(item_id: int):
    return {"id": item_id}


@router.get("/status")
def status():
    return {}


app.include_router(router, prefix="/v1")
