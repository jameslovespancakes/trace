/**
 * Handles a request.
 * @param {Request} req the request
 * @param {?Options} [opts] options
 * @returns {Promise<Response>}
 */
function handle(req, opts) {
  /** @type {Cache} */
  const cache = make();
  return fetch(req, cache, opts);
}

/** @type {Handler} */
const onError = (err) => err;

class Box {
  constructor() {
    /** @type {Store} */
    this.store = open();
  }
}

const text = "/** @type {Nope} */";

/** @returns {Lost} */

function lonely() {}
