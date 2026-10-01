export function add(a: number, b: number): number {
  return a + b;
}

export function scale(value: number, factor: number): number {
  return value * factor;
}

export const double = (x: number): number => add(x, x);
