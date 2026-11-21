import { useCallback, useId, useState, type FormEvent } from 'react'
import {
  createService,
  getService,
  listCustomerAssets,
  listCustomers,
  listServices,
  runOnAsset,
  startUsingService,
  stopRunningOnAsset,
  stopUsingService,
  type Asset,
  type ServiceDetail,
} from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { customerHref, go, serviceHref, servicesHref } from './route'
import { useAction, useLoad } from './useAction'

const loadServices = () => listServices()
const loadCustomers = () => listCustomers()

export function Services({ onSignedOut }: { onSignedOut: () => void }) {
  const { t, formatNumber } = useI18n()
  const services = useLoad(loadServices, onSignedOut)
  return (
    <div className="page">
      <h1>{t('services.title')}</h1>
      <Problem error={services.error} />
      {services.data?.length === 0 && <p className="empty">{t('services.empty')}</p>}
      {services.data && services.data.length > 0 && (
        <table className="data rows">
          <thead>
            <tr>
              <th>{t('services.name')}</th><th>{t('services.description')}</th>
              <th className="figure">{t('services.customers')}</th>
            </tr>
          </thead>
          <tbody>
            {services.data.map((s) => (
              // The name is the link; the rest of the row is a larger target for the mouse.
              <tr key={s.id} className={s.retired_at ? 'retired' : ''} onClick={() => go(serviceHref(s.id))}>
                <td>
                  <a href={serviceHref(s.id)}>{s.name}</a>
                  {s.retired_at && <> <span className="marker retired">{t('marker.retired')}</span></>}
                </td>
                <td className="wrap">{s.description || <span className="muted">—</span>}</td>
                <td className="figure">{formatNumber(s.customers)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <AddService onSignedOut={onSignedOut} />
    </div>
  )
}

function AddService({ onSignedOut }: { onSignedOut: () => void }) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const id = useId()

  function submit(event: FormEvent) {
    event.preventDefault()
    if (!name.trim()) return
    let created = ''
    void action.run(async () => {
      created = await createService(name.trim(), description.trim())
    }).then((ok) => { if (ok) go(serviceHref(created)) })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('services.add')}</h2>
      <form className="inline-form" onSubmit={submit}>
        <label>
          {t('services.name')}
          <input required value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="wide">
          {t('services.description')}
          <input value={description} onChange={(e) => setDescription(e.target.value)} />
        </label>
        <button type="submit" disabled={action.busy || !name.trim()}>{t('services.addSubmit')}</button>
      </form>
      <p className="hint">{t('services.addHint')}</p>
      <Problem error={action.error} />
    </section>
  )
}

export function ServicePage({ id, onSignedOut }: { id: string; onSignedOut: () => void }) {
  const { t, formatTime } = useI18n()
  const load = useCallback(() => getService(id), [id])
  const service = useLoad(load, onSignedOut)

  const s = service.data
  if (!s) {
    return (
      <div className="page">
        <a className="back" href={servicesHref}>{t('service.back')}</a>
        <Problem error={service.error} />
      </div>
    )
  }
  // A retired service stays on its tickets but gets no new links.
  const readOnly = Boolean(s.retired_at)
  return (
    <div className="page">
      <a className="back" href={servicesHref}>{t('service.back')}</a>
      <div className="title-row">
        <h1>{s.name}</h1>
        {s.retired_at && <span className="marker retired">{t('marker.retired')}</span>}
      </div>
      {s.description && <p className="description">{s.description}</p>}
      <Problem error={service.error} />
      {s.retired_at && <p className="notice">{t('service.retiredNotice', { date: formatTime(s.retired_at) })}</p>}
      <UsedBy service={s} readOnly={readOnly} onChanged={service.reload} onSignedOut={onSignedOut} />
      <RunsOn service={s} readOnly={readOnly} onChanged={service.reload} onSignedOut={onSignedOut} />
    </div>
  )
}

interface PartProps {
  service: ServiceDetail
  readOnly: boolean
  onChanged: () => Promise<void>
  onSignedOut: () => void
}

function UsedBy({ service: s, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t } = useI18n()
  const customers = useLoad(loadCustomers, onSignedOut)
  const action = useAction(onSignedOut)
  const [customerId, setCustomerId] = useState('')
  const id = useId()
  const candidates = customers.data?.filter((c) => !s.used_by.some((u) => u.id === c.id)) ?? []

  function add(event: FormEvent) {
    event.preventDefault()
    if (!customerId) return
    void action.run(() => startUsingService(s.id, customerId)).then(async (ok) => {
      if (!ok) return
      setCustomerId('')
      await onChanged()
    })
  }

  function remove(customer: string) {
    void action.run(() => stopUsingService(s.id, customer)).then(async (ok) => { if (ok) await onChanged() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('service.usedBy')}</h2>
      <ul className="items">
        {s.used_by.map((c) => (
          <li key={c.id}>
            <a href={customerHref(c.id)}>{c.name}</a>
            {c.is_help_desk && <> <span className="marker">{t('marker.helpDesk')}</span></>}
            {/* The help desk's own organization uses every service, always. */}
            {!readOnly && !c.is_help_desk && (
              <button type="button" className="link danger" disabled={action.busy}
                aria-label={t('service.removeCustomer', { name: c.name })} onClick={() => remove(c.id)}>
                {t('action.remove')}
              </button>
            )}
          </li>
        ))}
      </ul>
      {!readOnly && (
        <form className="inline-form" onSubmit={add}>
          <label>
            {t('service.customer')}
            <select value={customerId} disabled={!customers.data || candidates.length === 0}
              onChange={(e) => setCustomerId(e.target.value)}>
              <option value="">
                {!customers.data ? t('customer.loading')
                  : candidates.length === 0 ? t('service.everyCustomer') : t('customer.choose')}
              </option>
              {candidates.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
            </select>
          </label>
          <button type="submit" disabled={action.busy || !customerId}>{t('service.addCustomer')}</button>
        </form>
      )}
      <Problem error={customers.error ?? action.error} />
    </section>
  )
}

function RunsOn({ service: s, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t, formatTime } = useI18n()
  const customers = useLoad(loadCustomers, onSignedOut)
  const action = useAction(onSignedOut)
  const [customerId, setCustomerId] = useState('')
  const [assetId, setAssetId] = useState('')
  const id = useId()
  const loadAssets = useCallback(
    (): Promise<Asset[]> => (customerId ? listCustomerAssets(customerId) : Promise.resolve([])),
    [customerId])
  const assets = useLoad(loadAssets, onSignedOut)
  const candidates = customerId
    ? assets.data?.filter((a) => a.customer_id === customerId && !a.retired_at
      && !s.runs_on.some((r) => r.id === a.id)) ?? []
    : []

  function link(event: FormEvent) {
    event.preventDefault()
    if (!assetId) return
    void action.run(() => runOnAsset(s.id, assetId)).then(async (ok) => {
      if (!ok) return
      setAssetId('')
      await onChanged()
    })
  }

  function unlink(asset: string) {
    void action.run(() => stopRunningOnAsset(s.id, asset)).then(async (ok) => { if (ok) await onChanged() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('service.runsOn')}</h2>
      <p className="hint">{t('service.runsOnHint')}</p>
      {s.runs_on.length === 0 ? (
        <p className="empty">{t('service.runsOnEmpty')}</p>
      ) : (
        <table className="data">
          <thead>
            <tr>
              <th>{t('assets.kind')}</th><th>{t('assets.name')}</th><th>{t('service.owner')}</th>
              <th><span className="visually-hidden">{t('table.actions')}</span></th>
            </tr>
          </thead>
          <tbody>
            {s.runs_on.map((a) => (
              <tr key={a.id} className={a.retired_at ? 'retired' : ''}>
                <td>{a.kind}</td>
                <td>
                  {a.name}
                  {a.retired_at && (
                    <> <span className="marker retired">{t('assets.retired', { date: formatTime(a.retired_at) })}</span></>
                  )}
                </td>
                <td><a href={customerHref(a.customer_id)}>{a.customer_name}</a></td>
                <td className="row-actions">
                  {!readOnly && (
                    <button type="button" className="link danger" disabled={action.busy}
                      aria-label={t('links.unlinkNamed', { name: a.name })} onClick={() => unlink(a.id)}>
                      {t('action.unlink')}
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {!readOnly && (
        <form className="inline-form" onSubmit={link}>
          <label>
            {t('service.customer')}
            <select value={customerId} disabled={!customers.data}
              onChange={(e) => { setCustomerId(e.target.value); setAssetId('') }}>
              <option value="">{customers.data ? t('customer.choose') : t('customer.loading')}</option>
              {customers.data?.map((c) => (
                <option key={c.id} value={c.id}>{c.is_help_desk ? t('customer.helpDesk', { name: c.name }) : c.name}</option>
              ))}
            </select>
          </label>
          <label>
            {t('service.asset')}
            <select value={assetId} disabled={!customerId || candidates.length === 0}
              onChange={(e) => setAssetId(e.target.value)}>
              <option value="">
                {!customerId ? t('service.chooseCustomerFirst')
                  : candidates.length === 0 ? t('service.noAssets') : t('asset.choose')}
              </option>
              {candidates.map((a) => <option key={a.id} value={a.id}>{a.name} ({a.kind})</option>)}
            </select>
          </label>
          <button type="submit" disabled={action.busy || !assetId}>{t('service.linkAsset')}</button>
        </form>
      )}
      <Problem error={customers.error ?? assets.error ?? action.error} />
    </section>
  )
}
