"""Fixture (bridges gate, python message families)."""
import pika
import redis
import socketio
from celery import Celery
from kafka import KafkaProducer

sio = socketio.Server()
r = redis.Redis()
producer = KafkaProducer()
celery_app = Celery("fixture")


@sio.on("chat")
def on_chat(sid, data):
    return data


def emit_chat():
    sio.emit("chat", {"text": "hi"})


def publish_order():
    r.publish("orders", "x")


def send_event():
    producer.send("events", b"x")


def queue_task():
    celery_app.send_task("tasks.add", args=[1, 2])


def on_job(channel, method, properties, body):
    return body


def amqp():
    channel = pika.BlockingConnection().channel()
    channel.basic_publish(exchange="", routing_key="jobs", body=b"x")
    channel.basic_consume(queue="jobs", on_message_callback=on_job)
