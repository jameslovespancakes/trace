// Fixture (bridges gate, socket.io and node message families).
const { Server } = require("socket.io");
const { Kafka } = require("kafkajs");
const amqp = require("amqplib");

const io = new Server();

function onChat(msg) {
  return msg;
}

io.on("chat", onChat);

function emitChat() {
  io.emit("chat", "hi");
}

async function sendEvent() {
  const producer = new Kafka({ brokers: [] }).producer();
  await producer.send({ topic: "events", messages: [] });
}

async function queueJob() {
  const channel = await (await amqp.connect("amqp://localhost")).createChannel();
  channel.sendToQueue("jobs", Buffer.from("x"));
}

module.exports = { emitChat, sendEvent, queueJob };
