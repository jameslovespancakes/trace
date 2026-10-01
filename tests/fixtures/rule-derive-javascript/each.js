function each(list, fn) {
  for (const x of list) {
    fn(x);
  }
}

class Emitter {
  constructor() {
    this.listeners = [];
  }

  on(listener) {
    this.listeners.push(listener);
  }

  emit(value) {
    this.listeners.forEach((l) => l(value));
  }
}

function invoke(fn, self) {
  return fn.call(self, 1);
}

module.exports = { each, Emitter, invoke };
