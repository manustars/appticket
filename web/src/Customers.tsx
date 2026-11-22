import { useCallback, useId, useState, type FormEvent } from 'react'
import {
  addDomain,
  addRequester,
  createAsset,
  createCustomer,
  getCustomer,
  listAllCustomers,
  listCustomerAssets,
  listCustomerServices,
  listServices,
  offboardCustomer,
  removeDomain,
  renameCustomer,
  retireAsset,
  startUsingService,
  stopUsingService,
  updateAsset,
  type CustomerDetail,
} from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { customerHref, customersHref, defaultFilters, go, serviceHref, ticketsHref } from './route'
import { useSession } from './session'
import { useAction, useLoad } from './useAction'

/** Domains typed into one box, separated by spaces, commas or semicolons. */
const splitDomains = (text: string) => text.split(/[\s,;]+/).filter(Boolean)

const loadAll = () => listAllCustomers()
const loadServices = () => listServices()

export function Customers({ onSignedOut }: { onSignedOut: () => void }) {
  const { t, formatNumber } = useI18n()
  const customers = useLoad(loadAll, onSignedOut)
  const canManage = useSession().can('customers.manage')
  return (
    <div className="page">
      <h1>{t('customers.title')}</h1>
      <Problem error={customers.error} />
      {customers.data && (
        <table className="data rows">
          <thead>
            <tr>
              <th>{t('customers.name')}</th><th>{t('customers.domains')}</th>
              <th className="figure">{t('customers.requesters')}</th>
              <th className="figure">{t('customers.activeTickets')}</th>
            </tr>
          </thead>
          <tbody>
            {customers.data.map((c) => (
              // The name is the link; the rest of the row is a larger target for the mouse.
              <tr key={c.id} className={c.offboarded_at ? 'offboarded' : ''} onClick={() => go(customerHref(c.id))}>
                <td>
                  <a href={customerHref(c.id)}>{c.name}</a>
                  {c.is_help_desk && <> <span className="marker">{t('marker.helpDesk')}</span></>}
                  {c.offboarded_at && <> <span className="marker offboarded">{t('marker.offboarded')}</span></>}
                </td>
                <td className="wrap">{c.domains.length ? c.domains.join(', ') : <span className="muted">—</span>}</td>
                <td className="figure">{formatNumber(c.requesters)}</td>
                <td className="figure">{formatNumber(c.active_tickets)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {canManage && <AddCustomer onSignedOut={onSignedOut} />}
    </div>
  )
}

function AddCustomer({ onSignedOut }: { onSignedOut: () => void }) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [name, setName] = useState('')
  const [domains, setDomains] = useState('')
  const id = useId()

  function submit(event: FormEvent) {
    event.preventDefault()
    if (!name.trim()) return
    let created = ''
    void action.run(async () => {
      created = await createCustomer(name.trim(), splitDomains(domains))
    }).then((ok) => { if (ok) go(customerHref(created)) })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('customers.add')}</h2>
      <form className="inline-form" onSubmit={submit}>
        <label>
          {t('customers.newName')}
          <input required value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="wide">
          {t('customers.newDomains')}
          <input value={domains} aria-describedby={`${id}-hint`} placeholder={t('customer.domainPlaceholder')}
            onChange={(e) => setDomains(e.target.value)} />
        </label>
        <button type="submit" disabled={action.busy || !name.trim()}>{t('customers.addSubmit')}</button>
      </form>
      <p className="hint" id={`${id}-hint`}>{t('customers.newDomainsHint')}</p>
      <Problem error={action.error} />
    </section>
  )
}

interface PageProps {
  id: string
  /** Called after a change, so the lists follow it. */
  onChanged: () => void
  onSignedOut: () => void
}

export function CustomerPage({ id, onChanged, onSignedOut }: PageProps) {
  const { t, formatTime, formatNumber } = useI18n()
  const { can } = useSession()
  const load = useCallback(() => getCustomer(id), [id])
  const customer = useLoad(load, onSignedOut)
  const { reload } = customer
  const changed = useCallback(async () => {
    await reload()
    onChanged()
  }, [reload, onChanged])

  const c = customer.data
  if (!c) {
    return (
      <div className="page">
        <a className="back" href={customersHref}>{t('customer.back')}</a>
        <Problem error={customer.error} />
      </div>
    )
  }
  // An offboarded customer's history stays as it was: nothing on this page changes it. Otherwise
  // the customer itself is changed by those who manage customers, its assets and the services it
  // uses by those who manage the catalog.
  const offboarded = Boolean(c.offboarded_at)
  const readOnly = offboarded || !can('customers.manage')
  const catalogReadOnly = offboarded || !can('catalog.manage')
  return (
    <div className="page">
      <a className="back" href={customersHref}>{t('customer.back')}</a>
      <CustomerName customer={c} readOnly={readOnly} onChanged={changed} onSignedOut={onSignedOut} />
      <Problem error={customer.error} />
      {c.offboarded_at && (
        <p className="notice">{t('customer.offboardedNotice', { date: formatTime(c.offboarded_at) })}</p>
      )}
      <dl className="facts">
        <div>
          <dt>{t('customer.activeTickets')}</dt>
          <dd><a href={ticketsHref({ ...defaultFilters, customer: c.id })}>{formatNumber(c.active_tickets)}</a></dd>
        </div>
        <div><dt>{t('customer.requesters')}</dt><dd>{formatNumber(c.requesters)}</dd></div>
      </dl>
      <Domains customer={c} readOnly={readOnly} onChanged={changed} onSignedOut={onSignedOut} />
      <Requesters customer={c} readOnly={readOnly} onChanged={changed} onSignedOut={onSignedOut} />
      <CustomerAssets customer={c} readOnly={catalogReadOnly} onSignedOut={onSignedOut} />
      <CustomerServices customer={c} readOnly={catalogReadOnly} onSignedOut={onSignedOut} />
      {!readOnly && <Offboard customer={c} onChanged={changed} onSignedOut={onSignedOut} />}
    </div>
  )
}

interface PartProps {
  customer: CustomerDetail
  readOnly: boolean
  onChanged: () => Promise<void>
  onSignedOut: () => void
}

function CustomerName({ customer: c, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [name, setName] = useState<string | null>(null)
  const id = useId()

  function submit(event: FormEvent) {
    event.preventDefault()
    if (name === null || !name.trim()) return
    void action.run(() => renameCustomer(c.id, name.trim())).then(async (ok) => {
      if (!ok) return
      await onChanged()
      setName(null)
    })
  }

  return (
    <>
      {name === null ? (
        <div className="title-row">
          <h1>{c.name}</h1>
          {c.is_help_desk && <span className="marker">{t('marker.helpDesk')}</span>}
          {c.offboarded_at && <span className="marker offboarded">{t('marker.offboarded')}</span>}
          {!readOnly && (
            <button type="button" className="link" onClick={() => setName(c.name)}>{t('customer.rename')}</button>
          )}
        </div>
      ) : (
        <form className="inline-form rename" onSubmit={submit}>
          <label htmlFor={`${id}-name`}>{t('customer.newName')}</label>
          <input id={`${id}-name`} required value={name} autoFocus onChange={(e) => setName(e.target.value)} />
          <button type="submit" disabled={action.busy || !name.trim()}>{t('action.save')}</button>
          <button type="button" className="quiet" onClick={() => { setName(null); action.clear() }}>
            {t('action.cancel')}
          </button>
        </form>
      )}
      <Problem error={action.error} />
    </>
  )
}

function Domains({ customer: c, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [domain, setDomain] = useState('')
  const id = useId()

  function add(event: FormEvent) {
    event.preventDefault()
    if (!domain.trim()) return
    void action.run(() => addDomain(c.id, domain.trim())).then(async (ok) => {
      if (!ok) return
      setDomain('')
      await onChanged()
    })
  }

  function remove(d: string) {
    void action.run(() => removeDomain(c.id, d)).then(async (ok) => { if (ok) await onChanged() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('customer.domains')}</h2>
      <p className="hint">{t('customer.domainsHint')}</p>
      {c.domains.length === 0 ? (
        <p className="empty">{t('customer.noDomains')}</p>
      ) : (
        <ul className="chips">
          {c.domains.map((d) => (
            <li key={d}>
              {d}
              {!readOnly && (
                <button type="button" className="link danger" disabled={action.busy}
                  aria-label={t('customer.removeDomain', { domain: d })} onClick={() => remove(d)}>
                  {t('action.remove')}
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {!readOnly && (
        <form className="inline-form" onSubmit={add}>
          <label>
            {t('customer.domain')}
            <input value={domain} placeholder={t('customer.domainPlaceholder')} onChange={(e) => setDomain(e.target.value)} />
          </label>
          <button type="submit" disabled={action.busy || !domain.trim()}>{t('customer.addDomain')}</button>
        </form>
      )}
      <Problem error={action.error} />
    </section>
  )
}

function Requesters({ customer: c, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t, formatNumber } = useI18n()
  const action = useAction(onSignedOut)
  const [email, setEmail] = useState('')
  const [name, setName] = useState('')
  const id = useId()

  function add(event: FormEvent) {
    event.preventDefault()
    if (!email.trim()) return
    void action.run(() => addRequester(c.id, email.trim(), name.trim() || null)).then(async (ok) => {
      if (!ok) return
      setEmail('')
      setName('')
      await onChanged()
    })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('customer.requesters')}</h2>
      {c.requester_list.length === 0 ? (
        <p className="empty">{t('customer.noRequesters')}</p>
      ) : (
        <table className="data">
          <thead>
            <tr>
              <th>{t('customer.requesterEmail')}</th><th>{t('customer.requesterName')}</th>
              <th className="figure">{t('customer.requesterTickets')}</th>
            </tr>
          </thead>
          <tbody>
            {c.requester_list.map((r) => (
              <tr key={r.id}>
                <td>{r.email}</td>
                <td>{r.name ?? <span className="muted">—</span>}</td>
                <td className="figure">{formatNumber(r.tickets)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {!readOnly && (
        <form className="inline-form" onSubmit={add}>
          <label>
            {t('customer.requesterEmail')}
            <input type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
          </label>
          <label>
            {t('customer.requesterNameOptional')}
            <input value={name} onChange={(e) => setName(e.target.value)} />
          </label>
          <button type="submit" disabled={action.busy || !email.trim()}>{t('customer.addRequester')}</button>
        </form>
      )}
      <Problem error={action.error} />
    </section>
  )
}

function CustomerAssets({ customer: c, readOnly, onSignedOut }: Omit<PartProps, 'onChanged'>) {
  const { t, formatTime } = useI18n()
  const load = useCallback(() => listCustomerAssets(c.id), [c.id])
  const assets = useLoad(load, onSignedOut)
  const action = useAction(onSignedOut)
  // The asset being edited, with what is typed for it.
  const [editing, setEditing] = useState<{ id: string; kind: string; name: string } | null>(null)
  const [kind, setKind] = useState('')
  const [name, setName] = useState('')
  const id = useId()
  const kinds = [...new Set(assets.data?.map((a) => a.kind))].sort()

  function add(event: FormEvent) {
    event.preventDefault()
    if (!kind.trim() || !name.trim()) return
    void action.run(() => createAsset(c.id, kind.trim(), name.trim())).then(async (ok) => {
      if (!ok) return
      setKind('')
      setName('')
      await assets.reload()
    })
  }

  function save(event: FormEvent) {
    event.preventDefault()
    if (!editing || !editing.kind.trim() || !editing.name.trim()) return
    void action.run(() => updateAsset(editing.id, editing.kind.trim(), editing.name.trim())).then(async (ok) => {
      if (!ok) return
      setEditing(null)
      await assets.reload()
    })
  }

  function retire(assetId: string, assetName: string) {
    if (!window.confirm(t('assets.confirmRetire', { name: assetName }))) return
    void action.run(() => retireAsset(assetId)).then(async (ok) => { if (ok) await assets.reload() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('assets.title')}</h2>
      <Problem error={assets.error} />
      {assets.data?.length === 0 && <p className="empty">{t('assets.empty')}</p>}
      {assets.data && assets.data.length > 0 && (
        // The row being edited submits the form below by its id, so that the table sits in no form.
        <table className="data">
          <thead>
            <tr>
              <th>{t('assets.kind')}</th><th>{t('assets.name')}</th>
              <th><span className="visually-hidden">{t('table.actions')}</span></th>
            </tr>
          </thead>
          <tbody>
            {assets.data.map((a) => editing?.id === a.id ? (
              // Its own key: reusing the shown row would turn the clicked Edit button into Save,
              // and the click would then submit the form.
              <tr key={`${a.id}-edit`}>
                <td>
                  <input form={`${id}-edit`} aria-label={t('assets.kind')} required list={`${id}-kinds`} value={editing.kind}
                    onChange={(e) => setEditing({ ...editing, kind: e.target.value })} />
                </td>
                <td>
                  <input form={`${id}-edit`} aria-label={t('assets.name')} required value={editing.name}
                    onChange={(e) => setEditing({ ...editing, name: e.target.value })} />
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
                <td className="row-actions">
                  {!readOnly && !a.retired_at && (
                    <>
                      <button type="button" className="link" disabled={action.busy}
                        onClick={() => setEditing({ id: a.id, kind: a.kind, name: a.name })}>
                        {t('action.edit')}
                      </button>
                      <button type="button" className="link danger" disabled={action.busy}
                        onClick={() => retire(a.id, a.name)}>
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
      <datalist id={`${id}-kinds`}>{kinds.map((k) => <option key={k} value={k} />)}</datalist>
      {!readOnly && (
        <form className="inline-form" onSubmit={add}>
          <label>
            {t('assets.kind')}
            <input required list={`${id}-kinds`} value={kind} placeholder={t('assets.kindPlaceholder')}
              onChange={(e) => setKind(e.target.value)} />
          </label>
          <label>
            {t('assets.name')}
            <input required value={name} onChange={(e) => setName(e.target.value)} />
          </label>
          <button type="submit" disabled={action.busy || !kind.trim() || !name.trim()}>{t('assets.add')}</button>
        </form>
      )}
      <Problem error={action.error} />
    </section>
  )
}

function CustomerServices({ customer: c, readOnly, onSignedOut }: Omit<PartProps, 'onChanged'>) {
  const { t } = useI18n()
  const load = useCallback(() => listCustomerServices(c.id), [c.id])
  const used = useLoad(load, onSignedOut)
  const all = useLoad(loadServices, onSignedOut)
  const action = useAction(onSignedOut)
  const [serviceId, setServiceId] = useState('')
  const id = useId()
  const candidates = all.data?.filter((s) => !s.retired_at && !used.data?.some((u) => u.id === s.id)) ?? []

  function start(event: FormEvent) {
    event.preventDefault()
    if (!serviceId) return
    void action.run(() => startUsingService(serviceId, c.id)).then(async (ok) => {
      if (!ok) return
      setServiceId('')
      await used.reload()
    })
  }

  function stop(service: string) {
    void action.run(() => stopUsingService(service, c.id)).then(async (ok) => { if (ok) await used.reload() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('customerServices.title')}</h2>
      {c.is_help_desk && <p className="hint">{t('customerServices.helpDeskUsesAll')}</p>}
      <Problem error={used.error ?? all.error} />
      {used.data?.length === 0 && <p className="empty">{t('customerServices.empty')}</p>}
      {used.data && used.data.length > 0 && (
        <ul className="items">
          {used.data.map((s) => (
            <li key={s.id}>
              <a href={serviceHref(s.id)}>{s.name}</a>
              {s.retired_at && <> <span className="marker retired">{t('marker.retired')}</span></>}
              {!readOnly && !c.is_help_desk && (
                <button type="button" className="link danger" disabled={action.busy}
                  aria-label={t('customerServices.stopNamed', { name: s.name })} onClick={() => stop(s.id)}>
                  {t('customerServices.stop')}
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {!readOnly && !c.is_help_desk && (
        <form className="inline-form" onSubmit={start}>
          <label>
            {t('customerServices.service')}
            <select value={serviceId} disabled={!all.data || candidates.length === 0}
              onChange={(e) => setServiceId(e.target.value)}>
              <option value="">
                {all.data && candidates.length === 0 ? t('customerServices.noneLeft') : t('service.choose')}
              </option>
              {candidates.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
            </select>
          </label>
          <button type="submit" disabled={action.busy || !serviceId}>{t('customerServices.start')}</button>
        </form>
      )}
      <Problem error={action.error} />
    </section>
  )
}

function Offboard({ customer: c, onChanged, onSignedOut }: Omit<PartProps, 'readOnly'>) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)

  function offboard() {
    if (!window.confirm(t('customer.confirmOffboard', { name: c.name }))) return
    void action.run(() => offboardCustomer(c.id)).then(async (ok) => { if (ok) await onChanged() })
  }

  return (
    <section className="panel danger-zone">
      <div className="danger-row">
        <p className="hint">{t('customer.offboardHint')}</p>
        <button type="button" className="danger" disabled={action.busy} onClick={offboard}>
          {t('customer.offboard')}
        </button>
      </div>
      <Problem error={action.error} />
    </section>
  )
}
