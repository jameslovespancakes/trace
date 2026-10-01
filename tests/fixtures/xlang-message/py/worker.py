"""Fixture (P5, message): a Python subscriber."""
import redis

r = redis.Redis()


def handle_order(message):
    return message


def listen():
    pubsub = r.pubsub()
    pubsub.subscribe("orders.created", handle_order)  # published from JavaScript -> possible
    pubsub.subscribe("local.only", handle_order)  # negative control: published in Python only


def notify():
    r.publish("local.only", "x")  # same language -> no bridge
