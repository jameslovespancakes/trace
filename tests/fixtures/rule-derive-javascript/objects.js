// Object model rules: member functions of objects, computed members, the arguments object,
// receiver-binding invocations.
var names = ['GET', 'POST'].map(function (m) { return m.toLowerCase(); });
var obj = {};
names.forEach(function (n) {
  obj[n] = function (cb) { cb(); };
});
obj.viaName = function viaName(cb) { this.get(cb); };
function viaIndex(cb, k) { obj[k](cb); }
function invoke(cb) { cb(); }
function viaCall(x) { invoke.call(null, x); }
function viaApply() { invoke.apply(null, arguments); }
function viaBind(o, cb) { var bound = cb.bind(o); bound(); }
function rest(first) {
  var others = Array.prototype.slice.call(arguments, 1);
  others.forEach(function (fn) { fn(); });
}
var registry = {};
registry.add = function add(fn) { this.fns = fn; };
registry.run = function run() { var f = this.fns; f(); };
