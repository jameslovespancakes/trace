"""Fixture (bridges gate, python subprocess family): starts a repository script."""
import subprocess


def main():
    subprocess.run(["python", "procs/job.py"], check=True)
