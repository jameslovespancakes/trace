// Fixture (P5, js_ts): a TypeScript caller (resolved to math.d.ts by the TypeScript server).
import { add, Calc } from "./math";

export function total(): number {
  return add(1, 2) + new Calc().mul(2, 3);
}
