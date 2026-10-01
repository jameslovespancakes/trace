"""Fixture (round 2, http constant prefix): settings class whose attribute is the API prefix."""


class Settings:
    API_V1_STR: str = "/api/v1"
    PROJECT_NAME: str = "fixture"


class Rebound:
    PREFIX = "/one"
    PREFIX = "/two"  # bound twice: never a constant


settings = Settings()
other = Rebound()
