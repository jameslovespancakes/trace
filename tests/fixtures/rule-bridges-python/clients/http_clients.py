"""Fixture (bridges gate, python HTTP client families)."""
import urllib.request

import aiohttp
import httpx
import requests


def with_requests():
    return requests.get("http://svc.local/flask/users/1")


def with_httpx():
    return httpx.post("http://svc.local/fapi/items/1")


async def with_aiohttp():
    async with aiohttp.ClientSession() as session:
        return await session.get("http://svc.local/starlette/ping")


def with_urllib():
    return urllib.request.urlopen("http://svc.local/django/items/3/")
