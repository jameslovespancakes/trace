"""Fixture (P5, graphql): a Strawberry resolver for the same field (makes `posts` ambiguous)."""
import strawberry


@strawberry.type
class Query:
    @strawberry.field
    def posts(self) -> list[str]:
        return []

    def helper(self) -> int:
        """Negative control: not a field resolver."""
        return 1
