// Fixture (bridges gate, node ffi family): koffi symbol lookup.
const koffi = require("koffi");

const lib = koffi.load("./libnative.so");
const compress = lib.func("compress_buf", "int", ["int"]);

module.exports = { compress };
