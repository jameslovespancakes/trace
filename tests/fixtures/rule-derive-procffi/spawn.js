// Rule fixture (procffi derivation rules): import-bound module variables and records.
const cp = require("_proc");

function direct(file, args) {
  cp.spawn(file, args);
}

const parse = (command, args, options) => {
  const parsed = { command, args, options };
  return parsed;
};

const prepare = (file, args, options) => {
  const parsed = parse(file, args, options);
  file = parsed.command;
  return { file, args, options };
};

function start(file, args, options) {
  const parsed = prepare(file, args, options);
  return cp.spawn(parsed.file, parsed.args, parsed.options);
}

function startArgs(file, args) {
  const parsed = { file, args };
  return cp.spawn(parsed.args);
}

module.exports = { direct, start, startArgs };
