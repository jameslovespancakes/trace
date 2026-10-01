// Fixture (bridges gate, next family): the client of the filesystem route.
export async function loadOrder() {
  return fetch("/api/orders/9");
}
