// Fixture (bridges gate, node subprocess family).
const { spawn } = require("child_process");
const execa = require("execa");

function build() {
  spawn("python", ["procs/build.py"]);
}

async function deploy() {
  await execa("sh", ["procs/deploy.sh"]);
}

module.exports = { build, deploy };
