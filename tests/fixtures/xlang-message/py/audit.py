"""Fixture (P5, message): a second Python subscriber of the same topic (ambiguous)."""
import redis


def audit(message):
    return message


def listen_audit():
    redis.Redis().pubsub().subscribe("orders.created", audit)
