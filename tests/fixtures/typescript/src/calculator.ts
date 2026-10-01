import { add, double, scale } from "./math";

export interface Operation {
  run(value: number): number;
}

export class Doubler implements Operation {
  run(value: number): number {
    return double(value);
  }
}

export class Calculator {
  constructor(private readonly base: number) {}

  total(values: number[]): number {
    return values.reduce((acc, v) => add(acc, v), this.base);
  }

  scaled(values: number[], factor: number): number {
    return scale(this.total(values), factor);
  }
}
