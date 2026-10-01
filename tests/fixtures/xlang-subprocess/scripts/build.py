"""Fixture (P5, subprocess): a script started from JavaScript; it starts a Node tool itself."""
import subprocess


def main():
    subprocess.run(["node", "tools/serve.js", "--port", "8080"], check=True)  # -> tools/serve.js


if __name__ == "__main__":
    main()
