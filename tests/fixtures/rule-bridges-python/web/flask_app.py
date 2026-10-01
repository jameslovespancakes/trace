"""Fixture (bridges gate, flask family): a route registered with the route decorator."""
from flask import Flask

app = Flask(__name__)


@app.route("/flask/users/<int:user_id>", methods=["GET"])
def flask_user(user_id):
    return {"id": user_id}
