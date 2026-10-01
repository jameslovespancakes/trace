"""Fixture (bridges gate, starlette family): a route object."""
from starlette.responses import PlainTextResponse
from starlette.routing import Route


async def ping(request):
    return PlainTextResponse("pong")


routes = [Route("/starlette/ping", ping)]
