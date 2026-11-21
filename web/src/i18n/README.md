# UI messages

Every word the agents' UI shows lives in `locales/<language>.json`. English (`en.json`) is the
source. Every other file is a translation of it, with the same keys.

`npm run i18n` (also run by `npm run dev` and `npm run build`) checks every translation against
English. It fails with the file and key when a rule below is broken. It also writes
`catalog.generated.ts`, the types `t()` is checked with: a key that does not exist, or a missing or
unknown placeholder, does not compile. Commit the generated file with the JSON. In CI the script
does not write it and fails if it is out of date.

## Messages

- A message is one string: `"inbox.empty": "The inbox is empty."`.
- `{name}` is a placeholder, filled in by the code: `t('inbox.goToTicket', { number: 12 })`. A
  translation uses exactly the placeholders English uses. It may put them in any order.
- `<strong>…</strong>` marks words the code shows as an element, through
  `rich(key, { strong: (inner) => <strong>{inner}</strong> })`. A translation uses exactly the
  tags English uses, around whichever of its own words fit.
- A message that depends on a number is an object of plural forms, chosen by `{count}` through
  `Intl.PluralRules`. For example: `{ "one": "{count} ticket", "other": "{count} tickets" }`.
  - A translation gives the forms its own language has (Italian: `one`, `many`, `other`).
  - `other` is required; it is also the fallback for a missing form.
  - A form the language never uses is an error.
  - `{count}` may be left out of a form: `"one": "the other ticket"`.
- Keys are grouped by screen (`ticket.`, `inbox.`, `customers.`, …). Keep the domain words of
  `GLOSSARY.md` and their established translations: in Italian, Ticket, Richiedente, Operatore,
  Cliente, Assegnatario, Nota interna, Posta in arrivo, and so on.
- The core's error texts (`{error}` in its responses) are shown as the core sends them and are not
  in these files.

## Adding a message

1. Add the key to `en.json`.
2. Add it to every other `locales/*.json`. `npm run i18n` lists the missing ones.
3. Use it: `t('screen.key', { … })`. Run `npm run i18n` if your editor does not know the key yet.

To change a message's placeholders, change them in English first, then in every translation.

## Adding a language

1. Copy `en.json` to `locales/<code>.json`, using a two-letter code such as `de`, and translate it.
2. Add one line to `languages` in `locale.ts`, naming the language in itself:
   `de: { name: 'Deutsch', load: () => import('./locales/de.json') },`.

The language switcher then offers it. A browser that prefers that language gets it by default.
Only the chosen language's file is downloaded.
