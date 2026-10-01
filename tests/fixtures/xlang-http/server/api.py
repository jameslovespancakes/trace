"""Fixture (P5, http): Flask service exposing the same user route (makes it ambiguous)."""
from flask import Flask

app = Flask(__name__)


@app.route("/users/<int:user_id>", methods=["GET"])
def get_user_py(user_id):
    return {"id": user_id}


@app.route("/reports")
def reports():  # negative control: no client calls it
    return {}
