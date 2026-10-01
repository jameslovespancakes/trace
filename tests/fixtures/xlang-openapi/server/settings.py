"""Fixture (P5, openapi): configuration (the mount prefix is not a literal at the mount)."""
import os

API_PREFIX = os.environ.get("API_PREFIX", "/api")
