// Fixture (P5, graphql): resolver map (Apollo style).
function addPostImpl(parent, args) {
  return { id: "1", title: args.title };
}

const resolvers = {
  Query: {
    user: (parent, args) => ({ id: args.id }), // unique -> inferred
    posts() {
      return []; // also resolved in server/schema.py -> possible
    },
  },
  Mutation: {
    addPost: addPostImpl, // unique -> inferred
  },
};

module.exports = { resolvers };
