// Fixture (P5, message): a JavaScript publisher.
export async function createOrder(client, order) {
  await client.publish("orders.created", JSON.stringify(order)); // two Python subscribers -> possible
  await client.publish("nobody.listens", "x"); // negative control: no subscriber
}
