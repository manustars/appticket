// The asset kinds the help desk manages: each says whether its assets move with their holder or
// stay at their site. They are part of the catalog, changed by those who manage it.
import { useId, useState, type FormEvent } from 'react'
import { createAssetKind, listAssetKinds, updateAssetKind } from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { useAction, useLoad } from './useAction'

const loadKinds = () => listAssetKinds()

/** Whether a kind's assets move with their holder, as a select's value. */
const movesValue = (movesWithHolder: boolean) => (movesWithHolder ? 'holder' : 'site')

export function AdminKinds({ onSignedOut }: { onSignedOut: () => void }) {
  const { t } = useI18n()
  const kinds = useLoad(loadKinds, onSignedOut)
  const action = useAction(onSignedOut)
  // The kind being edited, with what is typed for it.
  const [editing, setEditing] = useState<{ id: string; name: string; movesWithHolder: boolean } | null>(null)
  const [name, setName] = useState('')
  const [movesWithHolder, setMovesWithHolder] = useState(false)
  const id = useId()

  function add(event: FormEvent) {
    event.preventDefault()
    if (!name.trim()) return
    void action.run(() => createAssetKind(name.trim(), movesWithHolder)).then(async (ok) => {
      if (!ok) return
      setName('')
      setMovesWithHolder(false)
      await kinds.reload()
    })
  }

  function save(event: FormEvent) {
    event.preventDefault()
    if (!editing || !editing.name.trim()) return
    void action.run(() => updateAssetKind(editing.id, editing.name.trim(), editing.movesWithHolder))
      .then(async (ok) => {
        if (!ok) return
        setEditing(null)
        await kinds.reload()
      })
  }

  const where = (moves: boolean) => (moves ? t('kinds.movesWithHolder') : t('kinds.staysAtSite'))
  const whereOptions = [false, true].map((moves) => (
    <option key={movesValue(moves)} value={movesValue(moves)}>{where(moves)}</option>
  ))
  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('kinds.title')}</h2>
      <p className="hint">{t('kinds.hint')}</p>
      <Problem error={kinds.error} />
      {kinds.data?.length === 0 && <p className="empty">{t('kinds.empty')}</p>}
      {kinds.data && kinds.data.length > 0 && (
        // The row being edited submits the form below by its id, so that the table sits in no form.
        <table className="data">
          <thead>
            <tr>
              <th>{t('kinds.name')}</th><th>{t('kinds.placement')}</th>
              <th><span className="visually-hidden">{t('table.actions')}</span></th>
            </tr>
          </thead>
          <tbody>
            {kinds.data.map((k) => editing?.id === k.id ? (
              // Its own key: reusing the shown row would turn the clicked Edit button into Save,
              // and the click would then submit the form.
              <tr key={`${k.id}-edit`}>
                <td>
                  <input form={`${id}-edit`} aria-label={t('kinds.name')} required value={editing.name}
                    onChange={(e) => setEditing({ ...editing, name: e.target.value })} />
                </td>
                <td>
                  <select form={`${id}-edit`} aria-label={t('kinds.placement')} value={movesValue(editing.movesWithHolder)}
                    onChange={(e) => setEditing({ ...editing, movesWithHolder: e.target.value === movesValue(true) })}>
                    {whereOptions}
                  </select>
                </td>
                <td className="row-actions">
                  <button type="submit" form={`${id}-edit`} disabled={action.busy}>{t('action.save')}</button>
                  <button type="button" className="quiet" onClick={() => { setEditing(null); action.clear() }}>
                    {t('action.cancel')}
                  </button>
                </td>
              </tr>
            ) : (
              <tr key={k.id}>
                <td>{k.name}</td>
                <td>{where(k.moves_with_holder)}</td>
                <td className="row-actions">
                  <button type="button" className="link" disabled={action.busy}
                    onClick={() => setEditing({ id: k.id, name: k.name, movesWithHolder: k.moves_with_holder })}>
                    {t('action.edit')}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {editing && <form id={`${id}-edit`} onSubmit={save} />}
      <form className="inline-form" onSubmit={add}>
        <label>
          {t('kinds.name')}
          <input required value={name} placeholder={t('kinds.namePlaceholder')}
            onChange={(e) => setName(e.target.value)} />
        </label>
        <label>
          {t('kinds.placement')}
          <select value={movesValue(movesWithHolder)}
            onChange={(e) => setMovesWithHolder(e.target.value === movesValue(true))}>
            {whereOptions}
          </select>
        </label>
        <button type="submit" disabled={action.busy || !name.trim()}>{t('kinds.add')}</button>
      </form>
      <Problem error={action.error} />
    </section>
  )
}
