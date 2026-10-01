"""Fixture (P5, user_contract): a service entry point reached through a queue nothing detects."""


def handle_job(payload):
    return payload


def unrelated():
    return None
