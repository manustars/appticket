// Importing customers, sites and requesters from CSV files: a preview first, then the import
// itself, and the rows left out to download, correct and import again.
import { useId, useState } from 'react'
import { applyImport, previewImport, Refused, type ImportFile, type ImportKind, type ImportReport, type ImportRow } from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { customersHref } from './route'
import { useAction } from './useAction'

/** The kinds this screen imports, in the order an import takes them, with their columns. */
const kinds: { kind: ImportKind; columns: string[] }[] = [
  {
    kind: 'customers',
    columns: ['name', 'external_system', 'external_name', 'legal_name', 'tax_id', 'tax_country',
      'address', 'phone', 'website', 'notes'],
  },
  { kind: 'sites', columns: ['customer', 'customer_external_system', 'customer_external_name', 'name', 'address'] },
  {
    kind: 'requesters',
    columns: ['customer', 'customer_external_system', 'customer_external_name', 'email_addresses', 'name',
      'phone_numbers', 'job_title', 'site', 'notes'],
  },
]

/** A file the browser could not read as UTF-8. */
class NotUtf8 extends Error {}

/** The file's text, its byte order mark kept so that the rows left out come back with it. */
async function readText(file: File): Promise<string> {
  try {
    return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(await file.arrayBuffer())
  } catch {
    throw new NotUtf8(file.name)
  }
}

/** Offers `text` as a file named `name` to save. */
function download(name: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: 'text/csv;charset=utf-8' }))
  const link = document.createElement('a')
  link.href = url
  link.download = name
  link.click()
  // The browser reads the file after the click has been handled, not during it.
  setTimeout(() => URL.revokeObjectURL(url), 10_000)
}

export function Import({ onSignedOut }: { onSignedOut: () => void }) {
  const { t } = useI18n()
  const id = useId()
  const action = useAction(onSignedOut)
  const [chosen, setChosen] = useState<Partial<Record<ImportKind, File>>>({})
  // The files as previewed: applying sends exactly these, so that what was seen is what is imported.
  const [previewed, setPreviewed] = useState<ImportFile[] | null>(null)
  const [report, setReport] = useState<ImportReport | null>(null)
  const [unreadable, setUnreadable] = useState<string | null>(null)

  function choose(kind: ImportKind, file: File | undefined) {
    setChosen((c) => ({ ...c, [kind]: file }))
    setPreviewed(null)
    setReport(null)
    setUnreadable(null)
    action.clear()
  }

  function preview() {
    setUnreadable(null)
    setReport(null)
    void action.run(async () => {
      const files: ImportFile[] = []
      for (const { kind } of kinds) {
        const file = chosen[kind]
        if (!file) continue
        try {
          files.push({ kind, content: await readText(file) })
        } catch (e) {
          if (e instanceof NotUtf8) {
            setUnreadable(e.message)
            return
          }
          throw e
        }
      }
      setReport(await previewImport(files))
      setPreviewed(files)
    })
  }

  function apply() {
    if (!previewed) return
    void action.run(async () => {
      setReport(await applyImport(previewed))
      setPreviewed(null)
    })
  }

  const nothingChosen = !kinds.some(({ kind }) => chosen[kind])
  // A request over the core's limit is refused before the core reads it, without its own words.
  const tooLarge = action.error instanceof Refused && action.error.status === 413
  return (
    <div className="page">
      <a className="back" href={customersHref}>{t('customer.back')}</a>
      <h1>{t('import.title')}</h1>
      <p className="description">{t('import.intro')}</p>
      <section className="panel" aria-labelledby={`${id}-files`}>
        <h2 id={`${id}-files`}>{t('import.files')}</h2>
        <div className="stacked">
          {kinds.map(({ kind, columns }) => (
            <label key={kind}>
              {t(`import.file.${kind}`)}
              <input type="file" accept=".csv,text/csv" aria-describedby={`${id}-${kind}`}
                onChange={(e) => choose(kind, e.target.files?.[0])} />
              <span className="hint" id={`${id}-${kind}`}>
                {t('import.columns', { columns: columns.join(', ') })}
              </span>
            </label>
          ))}
        </div>
        <p className="hint">{t('import.filesHint')}</p>
        <div className="import-actions">
          <button type="button" disabled={action.busy || nothingChosen} onClick={preview}>
            {t('import.preview')}
          </button>
          <button type="button" className="primary" disabled={action.busy || !previewed} onClick={apply}>
            {t('import.apply')}
          </button>
        </div>
        {unreadable && <p className="error" role="alert">{t('import.notUtf8', { file: unreadable })}</p>}
        {tooLarge ? <p className="error" role="alert">{t('import.tooLarge')}</p> : <Problem error={action.error} />}
      </section>
      {report && (
        <>
          <p className={report.applied ? 'notice done' : 'notice'} role="status">
            {report.applied ? t('import.applied') : t('import.previewed')}
          </p>
          {report.files.map((file) => (
            <FileResult key={file.kind} kind={file.kind} rows={file.rows} leftOut={file.left_out ?? null}
              applied={report.applied} />
          ))}
        </>
      )}
    </div>
  )
}

function FileResult({ kind, rows, leftOut, applied }: {
  kind: ImportKind
  rows: ImportRow[]
  leftOut: string | null
  applied: boolean
}) {
  const { t, formatNumber } = useI18n()
  const id = useId()
  const count = (outcome: ImportRow['outcome']) => rows.filter((r) => r.outcome === outcome).length
  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t(`import.file.${kind}`)}</h2>
      <ul className="import-counts">
        <li>{t(applied ? 'import.count.created' : 'import.count.create', { count: count('create') })}</li>
        <li>{t(applied ? 'import.count.updated' : 'import.count.update', { count: count('update') })}</li>
        <li>{t('import.count.unchanged', { count: count('unchanged') })}</li>
        <li>{t('import.count.leftOut', { count: count('left_out') })}</li>
      </ul>
      {leftOut && (
        <div className="import-left-out">
          <button type="button" onClick={() => download(`${kind}-left-out.csv`, leftOut)}>
            {t('import.downloadLeftOut')}
          </button>
          <span className="hint">{t('import.leftOutHint')}</span>
        </div>
      )}
      {rows.length > 0 && (
        <table className="data import-rows">
          <thead>
            <tr>
              <th className="figure">{t('import.line')}</th>
              <th>{t('import.row')}</th>
              <th>{t('import.outcome')}</th>
              <th>{t('import.detail')}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.line} className={`outcome-${row.outcome}`}>
                <td className="figure">{formatNumber(row.line)}</td>
                <td className="wrap">{row.label || <span className="muted">—</span>}</td>
                <td>{t(`import.outcome.${row.outcome}`)}</td>
                <td className="wrap">
                  {row.reason ?? (row.changes.length > 0 && t('import.changes', { columns: row.changes.join(', ') }))}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  )
}
