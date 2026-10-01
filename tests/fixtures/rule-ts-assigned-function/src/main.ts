import { App, Base, either, Exposed, first, makeHandler, Patched, table, tag, tagged, Twice } from './router'
import type { Handler } from './router'

function pick(): Handler {
  return (s) => s
}

const alias: Handler = first as Handler

export function main() {
  const app = new App()
  app.get('/a')
  app.post('/b')
  app.use('/c')
  const patched = new Patched()
  patched.run('x')
  let later: Handler
  later = (s) => s + '.'
  later('x')
  alias('x')
  const base: Base = new Base()
  base.hook('x')
  const twice = new Twice(true)
  twice.fn('x')
  let replaced: Handler = () => ''
  replaced = pick()
  replaced('x')
  const external = new Exposed()
  external.exec('x')
  const made = makeHandler('m')
  made('x')
  makeHandler('n')('y')
  either(true)('z')
  tag`a${1}`
  tagged`b`
  table.run('x')
}
