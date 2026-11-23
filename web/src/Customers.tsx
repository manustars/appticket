import { useCallback, useId, useMemo, useState, type FormEvent } from 'react'
import {
  addDomain,
  addExternalReference,
  addRequester,
  addSite,
  bringBackCustomer,
  createCustomer,
  getCustomer,
  listAllCustomers,
  listCustomerAssets,
  listCustomerServices,
  listExternalSystems,
  listServices,
  markKeyRequester,
  offboardCustomer,
  removeDomain,
  removeExternalReference,
  retireSite,
  startUsingService,
  stopUsingService,
  unmarkKeyRequester,
  updateCustomer,
  updateSite,
  type CustomerDetail,
} from './api/client'
import { CustomerAssets } from './Assets'
import { countries, countryName } from './countries'
import { useI18n } from './i18n/context'
import { DepartedMarker } from './DepartedMarker'
import { Problem } from './Problem'
import { requesterLabel } from './people'
import { customerHref, customersHref, defaultFilters, go, importHref, requesterHref, serviceHref, ticketsHref } from './route'
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
      <div className="title-row">
        <h1>{t('customers.title')}</h1>
        {canManage && <a href={importHref}>{t('customers.import')}</a>}
      </div>
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
  // An offboarded customer's history stays as it was: nothing on this page changes it, until those
  // who manage customers bring it back. Otherwise the customer itself is changed by those who
  // manage customers, its assets and the services it uses by those who manage the catalog.
  const offboarded = Boolean(c.offboarded_at)
  const readOnly = offboarded || !can('customers.manage')
  const catalogReadOnly = offboarded || !can('catalog.manage')
  return (
    <div className="page">
      <a className="back" href={customersHref}>{t('customer.back')}</a>
      <div className="title-row">
        <h1>{c.name}</h1>
        {c.is_help_desk && <span className="marker">{t('marker.helpDesk')}</span>}
        {c.offboarded_at && <span className="marker offboarded">{t('marker.offboarded')}</span>}
      </div>
      <Problem error={customer.error} />
      {c.offboarded_at && (
        <p className="notice">{t('customer.offboardedNotice', { date: formatTime(c.offboarded_at) })}</p>
      )}
      {offboarded && can('customers.manage') && (
        <BringBack customer={c} onChanged={changed} onSignedOut={onSignedOut} />
      )}
      <dl className="facts">
        <div>
          <dt>{t('customer.activeTickets')}</dt>
          <dd><a href={ticketsHref({ ...defaultFilters, customer: c.id })}>{formatNumber(c.active_tickets)}</a></dd>
        </div>
        <div><dt>{t('customer.requesters')}</dt><dd>{formatNumber(c.requesters)}</dd></div>
      </dl>
      <CustomerDetails customer={c} readOnly={readOnly} onChanged={changed} onSignedOut={onSignedOut} />
      <Domains customer={c} readOnly={readOnly} onChanged={changed} onSignedOut={onSignedOut} />
      <Sites customer={c} readOnly={readOnly} onChanged={changed} onSignedOut={onSignedOut} />
      <ExternalReferences customer={c} readOnly={readOnly} onChanged={changed} onSignedOut={onSignedOut} />
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

/** What is typed while the details are edited. */
interface DetailsDraft {
  name: string
  legalName: string
  taxId: string
  taxCountry: string
  address: string
  phone: string
  website: string
  notes: string
}

const none = <span className="muted">—</span>

function CustomerDetails({ customer: c, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t, locale } = useI18n()
  const action = useAction(onSignedOut)
  const [draft, setDraft] = useState<DetailsDraft | null>(null)
  const id = useId()
  const countryOptions = useMemo(() => countries(locale), [locale])

  function edit() {
    action.clear()
    setDraft({
      name: c.name,
      legalName: c.legal_name ?? '',
      taxId: c.tax_id ?? '',
      taxCountry: c.tax_country ?? '',
      address: c.address ?? '',
      phone: c.phone ?? '',
      website: c.website ?? '',
      notes: c.notes,
    })
  }

  function save(event: FormEvent) {
    event.preventDefault()
    if (!draft || !draft.name.trim()) return
    const details = {
      name: draft.name.trim(),
      legal_name: draft.legalName.trim() || null,
      tax_id: draft.taxId.trim() || null,
      tax_country: draft.taxCountry || null,
      address: draft.address.trim() || null,
      phone: draft.phone.trim() || null,
      website: draft.website.trim() || null,
      notes: draft.notes.trim(),
    }
    void action.run(() => updateCustomer(c.id, details)).then(async (ok) => {
      if (!ok) return
      setDraft(null)
      await onChanged()
    })
  }

  // A tax identifier always comes with its country: either one filled asks for the other.
  const taxStarted = Boolean(draft && (draft.taxId.trim() || draft.taxCountry))
  // A country no longer in the list still shows as it is stored.
  const shownCountries = draft?.taxCountry && !countryOptions.some((o) => o.code === draft.taxCountry)
    ? [{ code: draft.taxCountry, name: draft.taxCountry }, ...countryOptions]
    : countryOptions

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('customer.details')}</h2>
      {draft === null ? (
        <>
          <dl className="facts customer-facts">
            <div><dt>{t('customer.legalName')}</dt><dd>{c.legal_name ?? none}</dd></div>
            <div>
              <dt>{t('customer.taxId')}</dt>
              <dd>
                {c.tax_id && c.tax_country
                  ? t('customer.taxIdWithCountry', { id: c.tax_id, country: countryName(locale, c.tax_country) })
                  : none}
              </dd>
            </div>
            <div><dt>{t('customer.phone')}</dt><dd>{c.phone ?? none}</dd></div>
            <div>
              <dt>{t('customer.website')}</dt>
              <dd>{c.website ? <a href={c.website} target="_blank" rel="noopener noreferrer">{c.website}</a> : none}</dd>
            </div>
            <div className="wide"><dt>{t('customer.address')}</dt><dd className="lines">{c.address ?? none}</dd></div>
            <div className="wide"><dt>{t('customer.notes')}</dt><dd className="lines">{c.notes || none}</dd></div>
          </dl>
          {!readOnly && (
            <button type="button" className="link" onClick={edit}>{t('customer.edit')}</button>
          )}
        </>
      ) : (
        <form className="stacked" onSubmit={save}>
          <label>
            {t('customer.name')}
            <input required value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} />
          </label>
          <label>
            {t('customer.legalName')}
            <input value={draft.legalName} onChange={(e) => setDraft({ ...draft, legalName: e.target.value })} />
          </label>
          <div className="row">
            <label>
              {t('customer.taxId')}
              <input value={draft.taxId} required={taxStarted}
                onChange={(e) => setDraft({ ...draft, taxId: e.target.value })} />
            </label>
            <label>
              {t('customer.taxCountry')}
              <select value={draft.taxCountry} required={taxStarted}
                onChange={(e) => setDraft({ ...draft, taxCountry: e.target.value })}>
                <option value="">{t('customer.noCountry')}</option>
                {shownCountries.map((o) => <option key={o.code} value={o.code}>{o.name}</option>)}
              </select>
            </label>
          </div>
          <p className="hint">{t('customer.taxIdHint')}</p>
          <label>
            {t('customer.address')}
            <textarea rows={3} value={draft.address} onChange={(e) => setDraft({ ...draft, address: e.target.value })} />
          </label>
          <label>
            {t('customer.phone')}
            <input type="tel" value={draft.phone} onChange={(e) => setDraft({ ...draft, phone: e.target.value })} />
          </label>
          <label>
            {t('customer.website')}
            <input value={draft.website} placeholder={t('customer.websitePlaceholder')}
              onChange={(e) => setDraft({ ...draft, website: e.target.value })} />
          </label>
          <label>
            {t('customer.notes')}
            <textarea rows={4} value={draft.notes} onChange={(e) => setDraft({ ...draft, notes: e.target.value })} />
          </label>
          <p className="hint">{t('customer.notesHint')}</p>
          <div className="row">
            <button type="submit" disabled={action.busy || !draft.name.trim()}>{t('action.save')}</button>
            <button type="button" className="quiet" onClick={() => { setDraft(null); action.clear() }}>
              {t('action.cancel')}
            </button>
          </div>
        </form>
      )}
      <Problem error={action.error} />
    </section>
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

function ExternalReferences({ customer: c, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [system, setSystem] = useState('')
  const [name, setName] = useState('')
  const id = useId()
  // The systems in use across all customers, offered so that the same one is spelled alike.
  const systems = useLoad(listExternalSystems, onSignedOut)
  const { reload: reloadSystems } = systems

  function add(event: FormEvent) {
    event.preventDefault()
    if (!system.trim() || !name.trim()) return
    void action.run(() => addExternalReference(c.id, system.trim(), name.trim())).then(async (ok) => {
      if (!ok) return
      setName('')
      await Promise.all([onChanged(), reloadSystems()])
    })
  }

  function remove(referenceId: string) {
    void action.run(() => removeExternalReference(referenceId)).then(async (ok) => {
      if (ok) await Promise.all([onChanged(), reloadSystems()])
    })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('externalReferences.title')}</h2>
      <p className="hint">{t('externalReferences.hint')}</p>
      {c.external_references.length === 0 ? (
        <p className="empty">{t('externalReferences.empty')}</p>
      ) : (
        <table className="data">
          <thead>
            <tr>
              <th>{t('externalReferences.system')}</th><th>{t('externalReferences.name')}</th>
              <th><span className="visually-hidden">{t('table.actions')}</span></th>
            </tr>
          </thead>
          <tbody>
            {c.external_references.map((r) => (
              <tr key={r.id}>
                <td>{r.system}</td>
                <td>{r.name}</td>
                <td className="row-actions">
                  {!readOnly && (
                    <button type="button" className="link danger" disabled={action.busy}
                      aria-label={t('externalReferences.removeNamed', { name: r.name, system: r.system })}
                      onClick={() => remove(r.id)}>
                      {t('action.remove')}
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {!readOnly && (
        <form className="inline-form" onSubmit={add}>
          <label>
            {t('externalReferences.system')}
            <input required value={system} list={`${id}-systems`} onChange={(e) => setSystem(e.target.value)} />
          </label>
          <datalist id={`${id}-systems`}>
            {(systems.data ?? []).map((s) => <option key={s} value={s} />)}
          </datalist>
          <label>
            {t('externalReferences.name')}
            <input required value={name} onChange={(e) => setName(e.target.value)} />
          </label>
          <button type="submit" disabled={action.busy || !system.trim() || !name.trim()}>{t('externalReferences.add')}</button>
        </form>
      )}
      <Problem error={action.error} />
    </section>
  )
}

function Sites({ customer: c, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t, formatTime, formatNumber } = useI18n()
  const action = useAction(onSignedOut)
  // The site being edited, with what is typed for it.
  const [editing, setEditing] = useState<{ id: string; name: string; address: string } | null>(null)
  const [name, setName] = useState('')
  const [address, setAddress] = useState('')
  const id = useId()

  function add(event: FormEvent) {
    event.preventDefault()
    if (!name.trim() || !address.trim()) return
    void action.run(() => addSite(c.id, name.trim(), address.trim())).then(async (ok) => {
      if (!ok) return
      setName('')
      setAddress('')
      await onChanged()
    })
  }

  function save(event: FormEvent) {
    event.preventDefault()
    if (!editing || !editing.name.trim() || !editing.address.trim()) return
    void action.run(() => updateSite(editing.id, editing.name.trim(), editing.address.trim())).then(async (ok) => {
      if (!ok) return
      setEditing(null)
      await onChanged()
    })
  }

  /** What retiring the site asks, naming the requesters at it and the assets that lose it. */
  function retireQuestion(name: string, requesters: string[], assets: string[]): string {
    const who = requesters.join(', ')
    const what = assets.join(', ')
    if (assets.length === 0) {
      return requesters.length === 0
        ? t('sites.confirmRetire', { name })
        : t('sites.confirmRetireWithRequesters', { name, count: requesters.length, requesters: who })
    }
    if (requesters.length === 0) return t('sites.confirmRetireWithAssets', { name, count: assets.length, assets: what })
    return requesters.length === 1
      ? t('sites.confirmRetireWithRequesterAndAssets', { name, count: assets.length, requesters: who, assets: what })
      : t('sites.confirmRetireWithRequestersAndAssets', {
        name, count: assets.length, requesterCount: formatNumber(requesters.length), requesters: who, assets: what,
      })
  }

  function retire(siteId: string, siteName: string) {
    // The requesters and assets named are those at the site now, not when the page was loaded.
    let question = ''
    void action.run(async () => {
      const [fresh, assets] = await Promise.all([getCustomer(c.id), listCustomerAssets(c.id)])
      const requesters = fresh.requester_list.filter((r) => r.site_id === siteId).map((r) => requesterLabel(t, r.name, r.addresses[0]))
      // The active assets that lose the site: those at it now, also through a holder who is there
      // and leaves it, and those whose own site it is, even while they are with their holder
      // elsewhere.
      const leaving = assets
        .filter((a) => !a.retired_at && (a.at_site?.id === siteId || a.site?.id === siteId))
        .map((a) => a.name)
      question = retireQuestion(siteName, requesters, leaving)
    }).then((ok) => {
      if (!ok || !window.confirm(question)) return
      void action.run(() => retireSite(siteId)).then(async (done) => { if (done) await onChanged() })
    })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('sites.title')}</h2>
      <p className="hint">{t('sites.hint')}</p>
      {c.sites.length === 0 ? (
        <p className="empty">{t('sites.empty')}</p>
      ) : (
        // The row being edited submits the form below by its id, so that the table sits in no form.
        <table className="data">
          <thead>
            <tr>
              <th>{t('sites.name')}</th><th>{t('sites.address')}</th>
              <th><span className="visually-hidden">{t('table.actions')}</span></th>
            </tr>
          </thead>
          <tbody>
            {c.sites.map((s) => editing?.id === s.id ? (
              // Its own key: reusing the shown row would turn the clicked Edit button into Save.
              <tr key={`${s.id}-edit`}>
                <td>
                  <input form={`${id}-edit`} aria-label={t('sites.name')} required value={editing.name}
                    onChange={(e) => setEditing({ ...editing, name: e.target.value })} />
                </td>
                <td>
                  <input form={`${id}-edit`} aria-label={t('sites.address')} required value={editing.address}
                    onChange={(e) => setEditing({ ...editing, address: e.target.value })} />
                </td>
                <td className="row-actions">
                  <button type="submit" form={`${id}-edit`} disabled={action.busy}>{t('action.save')}</button>
                  <button type="button" className="quiet" onClick={() => { setEditing(null); action.clear() }}>
                    {t('action.cancel')}
                  </button>
                </td>
              </tr>
            ) : (
              <tr key={s.id} className={s.retired_at ? 'retired' : ''}>
                <td>
                  {s.name}
                  {s.retired_at && (
                    <> <span className="marker retired">{t('sites.retired', { date: formatTime(s.retired_at) })}</span></>
                  )}
                </td>
                <td>{s.address}</td>
                <td className="row-actions">
                  {!readOnly && (
                    <>
                      {/* A retired site can still be corrected; retiring is for good. */}
                      <button type="button" className="link" disabled={action.busy}
                        aria-label={t('sites.editNamed', { name: s.name })}
                        onClick={() => { setEditing({ id: s.id, name: s.name, address: s.address }); action.clear() }}>
                        {t('action.edit')}
                      </button>
                      {!s.retired_at && (
                        <button type="button" className="link danger" disabled={action.busy}
                          aria-label={t('sites.retireNamed', { name: s.name })}
                          onClick={() => retire(s.id, s.name)}>
                          {t('sites.retire')}
                        </button>
                      )}
                    </>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {editing && <form id={`${id}-edit`} onSubmit={save} />}
      {!readOnly && (
        <form className="inline-form" onSubmit={add}>
          <label>
            {t('sites.name')}
            <input required value={name} onChange={(e) => setName(e.target.value)} />
          </label>
          <label>
            {t('sites.address')}
            <input required value={address} placeholder={t('sites.addressPlaceholder')}
              onChange={(e) => setAddress(e.target.value)} />
          </label>
          <button type="submit" disabled={action.busy || !name.trim() || !address.trim()}>{t('sites.add')}</button>
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
  const siteName = (siteId: string | null | undefined) => c.sites.find((s) => s.id === siteId)?.name

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

  /** Marks the requester a key requester written to at `contact`, or unmarks them when empty. */
  function setKey(requesterId: string, contact: string) {
    void action
      .run(() => (contact ? markKeyRequester(requesterId, contact) : unmarkKeyRequester(requesterId)))
      .then(async (ok) => {
        if (ok) await onChanged()
      })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('customer.requesters')}</h2>
      {c.requester_list.length === 0 ? (
        <p className="empty">{t('customer.noRequesters')}</p>
      ) : (
        // Each requester's page has their addresses and details, and changes them.
        <>
          <p className="hint">{t('customer.keyRequestersHint')}</p>
          <table className="data">
            <thead>
              <tr>
                <th>{t('customer.requesterName')}</th><th>{t('customer.requesterAddresses')}</th>
                <th>{t('customer.requesterSite')}</th>
                <th>{t('customer.keyRequester')}</th>
                <th className="figure">{t('customer.requesterTickets')}</th>
              </tr>
            </thead>
            <tbody>
              {c.requester_list.map((r) => {
                const label = requesterLabel(t, r.name, r.addresses[0])
                return (
                  <tr key={r.id}>
                    <td>
                      <a href={requesterHref(r.id)}>{label}</a>
                      {r.departed && <DepartedMarker />}
                    </td>
                    <td>
                      <ul className="plain">{r.addresses.map((a) => <li key={a}>{a}</li>)}</ul>
                    </td>
                    <td>{siteName(r.site_id) ?? <span className="muted">—</span>}</td>
                    <td>
                      {readOnly || r.departed ? (
                        // A departed requester is not a key requester, and cannot be marked one.
                        r.contact_address
                          ? t('customer.keyRequesterAt', { address: r.contact_address })
                          : <span className="muted">—</span>
                      ) : (
                        <select aria-label={t('customer.keyRequesterOf', { requester: label })}
                          value={r.contact_address ?? ''} disabled={action.busy}
                          onChange={(e) => setKey(r.id, e.target.value)}>
                          <option value="">{t('customer.notKeyRequester')}</option>
                          {r.addresses.map((a) => (
                            // The column says "Key requester": the address alone keeps the list narrow.
                            <option key={a} value={a}>{a}</option>
                          ))}
                        </select>
                      )}
                    </td>
                    <td className="figure">{formatNumber(r.tickets)}</td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </>
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

function BringBack({ customer: c, onChanged, onSignedOut }: Omit<PartProps, 'readOnly'>) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)

  function bringBack() {
    if (!window.confirm(t('customer.confirmBringBack', { name: c.name }))) return
    void action.run(() => bringBackCustomer(c.id)).then(async (ok) => { if (ok) await onChanged() })
  }

  return (
    <section className="panel">
      <div className="danger-row">
        <p className="hint">{t('customer.bringBackHint')}</p>
        <button type="button" disabled={action.busy} onClick={bringBack}>{t('customer.bringBack')}</button>
      </div>
      <Problem error={action.error} />
    </section>
  )
}
