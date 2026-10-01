export type Handler = (path: string) => string
export interface Verb {
  (path: string): string
}

const METHODS = ['get', 'post'] as const

export class App {
  get!: Verb
  post!: Verb
  use: Handler
  constructor() {
    for (const method of METHODS) {
      this[method] = (path: string) => method + path
    }
    this.use = (path: string) => 'use' + path
  }
}

export function first(p: string): string {
  return p
}

export function second(p: string): string {
  return p + '!'
}

export class Patched {
  run!: Handler
  constructor() {
    this.run = first
  }
}

export class Base {
  hook: Handler = (p) => p
}

export class Derived extends Base {
  hook: Handler = (p) => p + p
}

export class Twice {
  fn!: Handler
  constructor(flag: boolean) {
    this.fn = first
    if (flag) this.fn = second
  }
}

export class Exposed {
  exec: Handler = (p) => p
}

export function makeHandler(prefix: string): Handler {
  return (p) => prefix + p
}

export function either(flag: boolean): Handler {
  if (flag) return first
  return second
}

export function tag(strings: TemplateStringsArray, ...values: unknown[]): string {
  return strings.join('') + values.length
}

export type Tag = (strings: TemplateStringsArray, ...values: unknown[]) => string
export const tagged: Tag = (strings) => strings.join('')

export const table = { run: first as Handler }
