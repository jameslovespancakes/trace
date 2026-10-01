// Member copying (rule: every member name of one parameter defined / stored on another
// parameter with the source's value is `copies_members`).

function merge(dest, src, redefine) {
  Object.getOwnPropertyNames(src).forEach(function forEachOwnPropertyName(name) {
    if (!redefine && Object.prototype.hasOwnProperty.call(dest, name)) {
      return;
    }
    var descriptor = Object.getOwnPropertyDescriptor(src, name);
    Object.defineProperty(dest, name, descriptor);
  });
  return dest;
}

function mergeDescriptors(destination, source, overwrite) {
  for (const name of Object.getOwnPropertyNames(source)) {
    if (!overwrite && Object.hasOwn(destination, name)) {
      continue;
    }
    const descriptor = Object.getOwnPropertyDescriptor(source, name);
    Object.defineProperty(destination, name, descriptor);
  }
  return destination;
}

function assignAll(target, source) {
  for (var key in source) {
    target[key] = source[key];
  }
  return target;
}

function fill(target, names) {
  for (const key of names) {
    target[key] = true;
  }
  return target;
}

function mixin(app, proto) {
  merge(app, proto, false);
  return app;
}

module.exports = merge;
