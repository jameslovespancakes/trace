import { table } from './router'
import type { Handler } from './router'

export function poke(target: unknown) {
  ;(target as any).exec = (p: string) => p + '?'
}

export function patchTable(k: string, f: Handler) {
  // @ts-expect-error: a key the literal's type does not declare
  table[k] = f
}
