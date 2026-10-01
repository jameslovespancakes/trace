// Fixture (P5, openapi): generated-style client helpers.
import { BASE } from "./config";

export async function readItem(id: number) {
  return fetch(`${BASE}/api/items/${id}`); // contract read_item -> inferred
}

export async function createItem() {
  return fetch("/api/items", { method: "POST" }); // two handlers named create_item -> possible
}

export async function health() {
  return fetch("/api/health"); // negative control: not in the contract, no route
}
