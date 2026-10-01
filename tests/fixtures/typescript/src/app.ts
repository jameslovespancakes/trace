import { add } from "./math";

type Handler = (value: number) => number;
const routes: Handler[] = [];

function on(handler: Handler): void {
  routes.push(handler);
}

export class Context {
  notFound = (): number => add(400, 4);
}

export class Base {
  compute(value: number): number {
    return add(value, 1);
  }
}

export class Plus extends Base {
  compute(value: number): number {
    return add(value, 2);
  }
}

const ctx = new Context();
on((v) => add(v, 3));
ctx.notFound();
ctx.notFound = (): number => 410;
export const initial = add(1, 2);
export const plus = new Plus().compute(initial);
