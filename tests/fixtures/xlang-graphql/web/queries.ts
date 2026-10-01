// Fixture (P5, graphql): client documents.
import { gql } from "@apollo/client";

export async function load(client: any) {
  const q = gql`
    query Load($id: ID!) {
      user(id: $id) { id name }
      posts { id }
    }
  `;
  const m = gql`
    mutation { addPost(title: "x") { id } }
  `;
  const n = gql`
    query { stats { count } }
  `; // negative control: no resolver, not in the schema
  return [await client.query({ query: q }), await client.mutate({ mutation: m }), n];
}
