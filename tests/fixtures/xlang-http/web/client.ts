// Fixture (P5, http): browser client.
import axios from "axios";

export async function loadUser(id: string) {
  return fetch(`/users/${id}`); // Express + Flask -> two possible rows
}

export async function createUser() {
  return axios.post("/users", { name: "x" }); // unique -> inferred
}

export async function status() {
  return fetch("http://localhost:3000/api/status"); // literal origin stripped -> inferred
}

export async function dynamic(url: string) {
  return fetch(url); // negative control: dynamic URL -> no bridge
}

export async function missing() {
  return fetch("/nothing/here"); // negative control: no route
}

export function notHttp(map: Map<string, string>) {
  return map.get("/users"); // negative control: not an HTTP client
}
