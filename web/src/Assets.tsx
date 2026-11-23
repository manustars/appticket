// A customer's assets, with their serial number, site and holder, and where each is: an asset whose
// kind moves with its holder is at its holder's site while it has a holder; any other asset is at
// its own site.
import { useCallback, useId, useState, type FormEvent } from 'react'
import {
  createAsset,
  listAssetKinds,
  listCustomerAssets,
  retireAsset,
  updateAsset,
  type Asset,
  type CustomerDetail,
} from './api/client'
import { useI18n } from './i18n/context'
import { requesterLabel } from './people'
import { Problem } from './Problem'
import { adminHref, requesterHref } from './route'
import { useAction, useLoad } from './useAction'

const none = <span className="muted">—</span>

/** The site the asset is at, following its kind, and whether that is its holder's. */
export function WhereItIs({ asset: a }: { asset: Asset }) {
  const { t } = useI18n()
  return (
    <>
      {a.at_site?.name ?? none}
      {a.moves_with_holder && a.holder && <span className="muted"> · {t('assets.withHolder')}</span>}
    </>
  )
}

/** The assets a requester holds, as their page lists them. */
export function HeldAssets({ assets }: { assets: Asset[] }) {
  const { t } = useI18n()
  if (assets.length === 0) return <p className="empty">{t('assets.noneHeld')}</p>
  return (
    <table className="data">
      <thead>
        <tr>
          <th>{t('assets.kind')}</th><th>{t('assets.name')}</th>
          <th>{t('assets.serialNumber')}</th><th>{t('assets.whereItIs')}</th>
        </tr>
      </thead>
      <tbody>
        {assets.map((a) => (
          <tr key={a.id}>
            <td>{a.kind}</td>
            <td>{a.name}</td>
            <td>{a.serial_number ?? none}</td>
            <td><WhereItIs asset={a} /></td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

/** What is picked and typed for the asset being edited. */
interface Draft {
  id: string
  kindId: string
  name: string
  serialNumber: string
  siteId: string
  holderId: string
}

const draftOf = (a: Asset): Draft => ({
  id: a.id,
  kindId: a.kind_id,
  name: a.name,
  serialNumber: a.serial_number ?? '',
  siteId: a.site?.id ?? '',
  holderId: a.holder?.id ?? '',
})

const loadKinds = () => listAssetKinds()

interface Props {
  customer: CustomerDetail
  readOnly: boolean
  onSignedOut: () => void
}

/** The customer's assets: added, corrected and retired by those who manage the catalog. */
export function CustomerAssets({ customer: c, readOnly, onSignedOut }: Props) {
  const { t, rich, formatTime } = useI18n()
  // Loaded again whenever the customer is, since retiring a site or a requester's departure
  // changes where its assets are and who holds them.
  const load = useCallback(() => listCustomerAssets(c.id), [c])
  const assets = useLoad(load, onSignedOut)
  const kinds = useLoad(loadKinds, onSignedOut)
  const action = useAction(onSignedOut)
  const [editing, setEditing] = useState<Draft | null>(null)
  const [kind, setKind] = useState('')
  const [name, setName] = useState('')
  const id = useId()
  // An asset is put only at an active site, and given only to a requester who has not departed.
  const sites = c.sites.filter((s) => !s.retired_at)
  const holders = c.requester_list.filter((r) => !r.departed)

  function add(event: FormEvent) {
    event.preventDefault()
    if (!kind || !name.trim()) return
    void action.run(() => createAsset(c.id, { kind_id: kind, name: name.trim() })).then(async (ok) => {
      if (!ok) return
      setKind('')
      setName('')
      await assets.reload()
    })
  }

  function save(event: FormEvent) {
    event.preventDefault()
    if (!editing || !editing.kindId || !editing.name.trim()) return
    const fields = {
      kind_id: editing.kindId,
      name: editing.name.trim(),
      serial_number: editing.serialNumber.trim() || null,
      site_id: editing.siteId || null,
      holder_id: editing.holderId || null,
    }
    void action.run(() => updateAsset(editing.id, fields)).then(async (ok) => {
      if (!ok) return
      setEditing(null)
      await assets.reload()
    })
  }

  const kindOptions = kinds.data?.map((k) => <option key={k.id} value={k.id}>{k.name}</option>)

  function retire(a: Asset) {
    const question = a.holder
      ? t('assets.confirmRetireHeld', { name: a.name, holder: requesterLabel(t, a.holder.name, a.holder.address) })
      : t('assets.confirmRetire', { name: a.name })
    if (!window.confirm(question)) return
    void action.run(() => retireAsset(a.id)).then(async (ok) => { if (ok) await assets.reload() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('assets.title')}</h2>
      <p className="hint">{t('assets.hint')}</p>
      <Problem error={assets.error} />
      <Problem error={kinds.error} />
      {assets.data?.length === 0 && <p className="empty">{t('assets.empty')}</p>}
      {assets.data && assets.data.length > 0 && (
        // The row being edited submits the form below by its id, so that the table sits in no form.
        <table className="data assets">
          <thead>
            <tr>
              <th>{t('assets.kind')}</th><th>{t('assets.name')}</th><th>{t('assets.serialNumber')}</th>
              <th>{t('assets.whereItIs')}</th><th>{t('assets.holder')}</th>
              <th><span className="visually-hidden">{t('table.actions')}</span></th>
            </tr>
          </thead>
          <tbody>
            {assets.data.map((a) => editing?.id === a.id ? (
              // Its own key: reusing the shown row would turn the clicked Edit button into Save,
              // and the click would then submit the form.
              <tr key={`${a.id}-edit`}>
                <td>
                  <select form={`${id}-edit`} aria-label={t('assets.kind')} required value={editing.kindId}
                    onChange={(e) => setEditing({ ...editing, kindId: e.target.value })}>
                    {kindOptions}
                  </select>
                </td>
                <td>
                  <input form={`${id}-edit`} aria-label={t('assets.name')} required value={editing.name}
                    onChange={(e) => setEditing({ ...editing, name: e.target.value })} />
                </td>
                <td>
                  <input form={`${id}-edit`} aria-label={t('assets.serialNumber')} value={editing.serialNumber}
                    onChange={(e) => setEditing({ ...editing, serialNumber: e.target.value })} />
                </td>
                <td>
                  <select form={`${id}-edit`} aria-label={t('assets.ownSite')} value={editing.siteId}
                    onChange={(e) => setEditing({ ...editing, siteId: e.target.value })}>
                    <option value="">{t('assets.noSite')}</option>
                    {sites.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
                  </select>
                </td>
                <td>
                  <select form={`${id}-edit`} aria-label={t('assets.holder')} value={editing.holderId}
                    onChange={(e) => setEditing({ ...editing, holderId: e.target.value })}>
                    <option value="">{t('assets.noHolder')}</option>
                    {holders.map((r) => (
                      <option key={r.id} value={r.id}>{requesterLabel(t, r.name, r.addresses[0])}</option>
                    ))}
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
              <tr key={a.id} className={a.retired_at ? 'retired' : ''}>
                <td>{a.kind}</td>
                <td>
                  {a.name}
                  {a.retired_at && (
                    <> <span className="marker retired">{t('assets.retired', { date: formatTime(a.retired_at) })}</span></>
                  )}
                </td>
                <td>{a.serial_number ?? none}</td>
                <td><WhereItIs asset={a} /></td>
                <td>
                  {a.holder
                    ? <a href={requesterHref(a.holder.id)}>{requesterLabel(t, a.holder.name, a.holder.address)}</a>
                    : none}
                </td>
                <td className="row-actions">
                  {!readOnly && !a.retired_at && (
                    <>
                      <button type="button" className="link" disabled={action.busy}
                        aria-label={t('assets.editNamed', { name: a.name })}
                        onClick={() => { setEditing(draftOf(a)); action.clear() }}>
                        {t('action.edit')}
                      </button>
                      <button type="button" className="link danger" disabled={action.busy}
                        aria-label={t('assets.retireNamed', { name: a.name })}
                        onClick={() => retire(a)}>
                        {t('assets.retire')}
                      </button>
                    </>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {editing && <form id={`${id}-edit`} onSubmit={save} />}
      {!readOnly && kinds.data?.length === 0 && (
        <p className="hint">
          {rich('assets.noKinds', { link: (inner) => <a href={adminHref('kinds')}>{inner}</a> })}
        </p>
      )}
      {!readOnly && (
        <form className="inline-form" onSubmit={add}>
          <label>
            {t('assets.kind')}
            <select required value={kind} onChange={(e) => setKind(e.target.value)}>
              <option value="" disabled>{t('assets.chooseKind')}</option>
              {kindOptions}
            </select>
          </label>
          <label>
            {t('assets.name')}
            <input required value={name} onChange={(e) => setName(e.target.value)} />
          </label>
          <button type="submit" disabled={action.busy || !kind || !name.trim()}>{t('assets.add')}</button>
        </form>
      )}
      <Problem error={action.error} />
    </section>
  )
}
