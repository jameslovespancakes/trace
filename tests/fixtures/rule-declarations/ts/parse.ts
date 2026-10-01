export function parse(input: string): number;
export function parse(input: number): number;
export function parse(input: string | number): number {
  return typeof input === "string" ? input.length : input;
}

export function total(values: string[]): number {
  return values.map((v) => parse(v)).reduce((a, b) => a + b, 0);
}
