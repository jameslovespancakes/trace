import { Calculator, Doubler } from "./calculator";

export function main(): number {
  const calc = new Calculator(1);
  const op = new Doubler();
  return op.run(calc.scaled([1, 2, 3], 2));
}
