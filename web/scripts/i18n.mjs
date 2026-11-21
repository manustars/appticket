// Checks the UI's message catalogs (src/i18n/locales/*.json) against the English source and writes
// src/i18n/catalog.generated.ts, the types `t()` is checked with. Runs before every build and dev
// server; `npm run i18n` runs it alone.
//
// With --check, or when $CI is set, it writes nothing and fails if the generated file is stale.
// Exits 1, naming each offending key, when a catalog breaks a rule (see src/i18n/README.md).
import { readFileSync, readdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'i18n')
const localesDir = join(root, 'locales')
const generatedPath = join(root, 'catalog.generated.ts')
const source = 'en'
const checkOnly = process.argv.includes('--check') || Boolean(process.env.CI)

const errors = []
const fail = (file, key, text) => errors.push(`${file}: "${key}": ${text}`)

function read(locale) {
  const file = `${locale}.json`
  let catalog
  try {
    catalog = JSON.parse(readFileSync(join(localesDir, file), 'utf8'))
  } catch (e) {
    errors.push(`${file}: not valid JSON: ${e.message}`)
    return null
  }
  if (typeof catalog !== 'object' || catalog === null || Array.isArray(catalog)) {
    errors.push(`${file}: must be an object of messages`)
    return null
  }
  return catalog
}

const isPlural = (message) => typeof message === 'object' && message !== null && !Array.isArray(message)

/** The categories Intl.PluralRules can select for a language. */
const categories = (locale) => new Set(new Intl.PluralRules(locale).resolvedOptions().pluralCategories)

/** A message's forms, after checking its shape; null when it has none usable. */
function forms(locale, key, message) {
  const file = `${locale}.json`
  if (typeof message === 'string') return [message]
  if (!isPlural(message)) {
    fail(file, key, 'must be a string, or an object of plural forms (arrays and numbers are not messages)')
    return null
  }
  const known = categories(locale)
  let ok = true
  for (const [category, form] of Object.entries(message)) {
    if (!known.has(category)) {
      fail(file, key, `plural form "${category}" is never used in this language (it has: ${[...known].join(', ')})`)
      ok = false
    }
    if (typeof form !== 'string') {
      fail(file, key, `plural form "${category}" must be a string`)
      ok = false
    }
  }
  if (!('other' in message)) {
    fail(file, key, 'plural forms must include "other"')
    ok = false
  }
  return ok ? Object.values(message) : null
}

/** The `{placeholder}` and `<tag>` names a message uses, after checking its tags pair up. */
function names(locale, key, texts) {
  const placeholders = new Set()
  const tags = new Set()
  for (const text of texts) {
    for (const m of text.matchAll(/\{(\w+)\}/g)) placeholders.add(m[1])
    const opened = [...text.matchAll(/<(\w+)>/g)].map((m) => m[1])
    const closed = [...text.matchAll(/<\/(\w+)>/g)].map((m) => m[1])
    for (const tag of opened) tags.add(tag)
    if (opened.sort().join() !== closed.sort().join()) {
      fail(`${locale}.json`, key, `tags do not pair up: <${opened.join('>, <')}> opened, </${closed.join('>, </')}> closed`)
    }
  }
  return { placeholders, tags }
}

const list = (set, open, close) => (set.size ? [...set].sort().map((n) => `${open}${n}${close}`).join(', ') : 'none')
const same = (a, b) => a.size === b.size && [...a].every((x) => b.has(x))

// ---- English, the source. ----
const en = read(source)
const shapes = new Map() // key -> { plural, placeholders, tags }
if (en) {
  for (const [key, message] of Object.entries(en)) {
    const texts = forms(source, key, message)
    if (texts) shapes.set(key, { plural: isPlural(message), ...names(source, key, texts) })
  }
}

// ---- Every translation, against English. ----
const locales = readdirSync(localesDir).filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5)).sort()
for (const locale of locales.filter((l) => l !== source)) {
  const catalog = read(locale)
  if (!catalog || !en) continue
  const file = `${locale}.json`
  for (const key of Object.keys(en)) if (!(key in catalog)) fail(file, key, 'missing; English has it')
  for (const [key, message] of Object.entries(catalog)) {
    const shape = shapes.get(key)
    if (!(key in en)) {
      fail(file, key, 'not in English; remove it, or add it to en.json first')
      continue
    }
    const texts = forms(locale, key, message)
    if (!texts || !shape) continue
    if (isPlural(message) !== shape.plural) {
      fail(file, key, shape.plural ? 'English has plural forms here; give plural forms too' : 'English has a single string here; give a single string too')
      continue
    }
    const own = names(locale, key, texts)
    if (!same(own.placeholders, shape.placeholders)) {
      fail(file, key, `uses placeholders ${list(own.placeholders, '{', '}')}; English uses ${list(shape.placeholders, '{', '}')}`)
    }
    if (!same(own.tags, shape.tags)) {
      fail(file, key, `uses tags ${list(own.tags, '<', '>')}; English uses ${list(shape.tags, '<', '>')}`)
    }
  }
}

// ---- The types `t()` is checked with. ----
const union = (set) => (set.size ? [...set].sort().map((n) => JSON.stringify(n)).join(' | ') : 'never')
const lines = [...shapes].map(([key, s]) => {
  const placeholders = new Set(s.placeholders)
  if (s.plural) placeholders.add('count')
  return `  ${JSON.stringify(key)}: { plural: ${s.plural}; placeholders: ${union(placeholders)}; tags: ${union(s.tags)} }`
})
const generated = `// Generated by scripts/i18n.mjs from locales/${source}.json; do not edit. Run \`npm run i18n\` after
// changing ${source}.json.

/** Per message: whether it has plural forms, the placeholders it needs (\`count\` for a plural), its tags. */
export interface Messages {
${lines.join('\n')}
}
`

let current = ''
try {
  current = readFileSync(generatedPath, 'utf8')
} catch {
  // Not generated yet.
}
if (errors.length === 0 && current !== generated) {
  if (checkOnly) errors.push('src/i18n/catalog.generated.ts is out of date with en.json: run `npm run i18n` and commit it')
  else writeFileSync(generatedPath, generated)
}

if (errors.length) {
  console.error(`i18n: ${errors.length} problem(s) in src/i18n/locales:`)
  for (const e of errors) console.error(`  ${e}`)
  process.exit(1)
}
console.log(`i18n: ${locales.join(', ')} ok, ${shapes.size} messages`)
