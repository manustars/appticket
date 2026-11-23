import { useCallback, useId, useState, type FormEvent } from 'react'
import {
  addRequesterAddress,
  bringBackRequester,
  departRequester,
  getCustomer,
  getRequester,
  listTickets,
  removeRequesterAddress,
  updateRequester,
  type CustomerDetail,
  type RequesterDetail,
} from './api/client'
import { useI18n } from './i18n/context'
import { requesterLabel } from './people'
import { Problem } from './Problem'
import { customerHref, customersHref } from './route'
import { useSession } from './session'
import { TicketTable } from './Tickets'
import { useAction, useLoad } from './useAction'

interface PageProps {
  id: string
  /** Called after a change, so the lists follow it. */
  onChanged: () => void
  onSignedOut: () => void
}

/** A requester's page: their addresses, details and tickets. */
export function RequesterPage({ id, onChanged, onSignedOut }: PageProps) {
  const { t, formatTime } = useI18n()
  const { can } = useSession()
  const load = useCallback(() => getRequester(id), [id])
  const requester = useLoad(load, onSignedOut)
  const loadTickets = useCallback(() => listTickets({ requester: id }), [id])
  const tickets = useLoad(loadTickets, onSignedOut)
  const { reload } = requester
  const changed = useCallback(async () => {
    await reload()
    onChanged()
  }, [reload, onChanged])

  const r = requester.data
  if (!r) {
    return (
      <div className="page">
        <a className="back" href={customersHref}>{t('customer.back')}</a>
        <Problem error={requester.error} />
      </div>
    )
  }
  // An offboarded customer's requesters stay as they were.
  const readOnly = r.customer.offboarded || !can('customers.manage')
  return (
    <div className="page">
      <a className="back" href={customerHref(r.customer.id)}>{t('requester.back', { customer: r.customer.name })}</a>
      <div className="title-row">
        <h1>{requesterLabel(t, r.name, r.addresses[0])}</h1>
        {r.departed_at && (
          <span className="marker departed">{t('requester.departedOn', { date: formatTime(r.departed_at) })}</span>
        )}
        {r.customer.offboarded && <span className="marker offboarded">{t('marker.offboarded')}</span>}
      </div>
      {r.departed_at && <p className="notice">{t('requester.departedNotice')}</p>}
      <Problem error={requester.error} />
      <Details requester={r} readOnly={readOnly} onChanged={changed} onSignedOut={onSignedOut} />
      <Addresses requester={r} readOnly={readOnly} onChanged={changed} onSignedOut={onSignedOut} />
      {!readOnly && <Departure requester={r} onChanged={changed} onSignedOut={onSignedOut} />}
      <section className="panel" aria-labelledby={`${id}-tickets`}>
        <h2 id={`${id}-tickets`}>{t('requester.tickets')}</h2>
        <Problem error={tickets.error} />
        {tickets.data?.length === 0 && <p className="empty">{t('requester.noTickets')}</p>}
        {tickets.data && tickets.data.length > 0 && <TicketTable tickets={tickets.data} />}
      </section>
    </div>
  )
}

interface PartProps {
  requester: RequesterDetail
  readOnly: boolean
  onChanged: () => Promise<void>
  onSignedOut: () => void
}

/** What is typed while the details are edited. */
interface Draft {
  name: string
  phones: string[]
  jobTitle: string
  notes: string
  siteId: string
}

function Details({ requester: r, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [draft, setDraft] = useState<Draft | null>(null)
  // The customer's sites, to choose from while editing.
  const [customer, setCustomer] = useState<CustomerDetail | null>(null)
  const id = useId()

  function edit() {
    action.clear()
    setDraft({
      name: r.name ?? '',
      phones: r.phone_numbers.length ? [...r.phone_numbers] : [''],
      jobTitle: r.job_title ?? '',
      notes: r.notes,
      siteId: r.site?.id ?? '',
    })
    void action.run(() => getCustomer(r.customer.id).then(setCustomer))
  }

  function save(event: FormEvent) {
    event.preventDefault()
    if (!draft) return
    const details = {
      name: draft.name.trim() || null,
      // Empty rows are the ones the agent did not fill: they are not numbers.
      phone_numbers: draft.phones.map((p) => p.trim()).filter(Boolean),
      job_title: draft.jobTitle.trim() || null,
      notes: draft.notes.trim(),
      site_id: draft.siteId || null,
    }
    void action.run(() => updateRequester(r.id, details)).then(async (ok) => {
      if (!ok) return
      setDraft(null)
      await onChanged()
    })
  }

  const setPhone = (index: number, value: string) =>
    draft && setDraft({ ...draft, phones: draft.phones.map((p, i) => (i === index ? value : p)) })
  const activeSites = customer?.sites.filter((s) => !s.retired_at) ?? []

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('requester.details')}</h2>
      {draft === null ? (
        <>
          <dl className="facts requester-facts">
            <div><dt>{t('requester.name')}</dt><dd>{r.name ?? <span className="muted">—</span>}</dd></div>
            <div>
              <dt>{t('requester.phones')}</dt>
              <dd>
                {r.phone_numbers.length === 0 ? <span className="muted">—</span> : (
                  <ul className="plain">{r.phone_numbers.map((p, i) => <li key={i}>{p}</li>)}</ul>
                )}
              </dd>
            </div>
            <div><dt>{t('requester.jobTitle')}</dt><dd>{r.job_title ?? <span className="muted">—</span>}</dd></div>
            <div><dt>{t('requester.site')}</dt><dd>{r.site?.name ?? <span className="muted">—</span>}</dd></div>
            <div className="wide">
              <dt>{t('requester.notes')}</dt>
              <dd className="notes">{r.notes || <span className="muted">—</span>}</dd>
            </div>
          </dl>
          {!readOnly && (
            <button type="button" className="link" onClick={edit}>{t('requester.edit')}</button>
          )}
        </>
      ) : (
        <form className="stacked" onSubmit={save}>
          <label>
            {t('requester.name')}
            <input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} />
          </label>
          <fieldset>
            <legend>{t('requester.phones')}</legend>
            {draft.phones.map((phone, index) => (
              <div className="row" key={index}>
                <input type="tel" value={phone} aria-label={t('requester.phoneNumber', { position: index + 1 })}
                  onChange={(e) => setPhone(index, e.target.value)} />
                <button type="button" className="link danger"
                  aria-label={t('requester.removePhoneNumber', { position: index + 1 })}
                  onClick={() => setDraft({ ...draft, phones: draft.phones.filter((_, i) => i !== index) })}>
                  {t('action.remove')}
                </button>
              </div>
            ))}
            <button type="button" className="link" onClick={() => setDraft({ ...draft, phones: [...draft.phones, ''] })}>
              {t('requester.addPhoneNumber')}
            </button>
          </fieldset>
          <label>
            {t('requester.jobTitle')}
            <input value={draft.jobTitle} onChange={(e) => setDraft({ ...draft, jobTitle: e.target.value })} />
          </label>
          <label>
            {t('requester.site')}
            <select value={draft.siteId} onChange={(e) => setDraft({ ...draft, siteId: e.target.value })}>
              <option value="">{t('requester.noSite')}</option>
              {activeSites.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
            </select>
          </label>
          <label>
            {t('requester.notes')}
            <textarea rows={4} value={draft.notes} onChange={(e) => setDraft({ ...draft, notes: e.target.value })} />
          </label>
          <p className="hint">{t('requester.notesHint')}</p>
          <div className="row">
            <button type="submit" disabled={action.busy}>{t('action.save')}</button>
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

function Addresses({ requester: r, readOnly, onChanged, onSignedOut }: PartProps) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [address, setAddress] = useState('')
  const id = useId()

  function add(event: FormEvent) {
    event.preventDefault()
    if (!address.trim()) return
    void action.run(() => addRequesterAddress(r.id, address.trim())).then(async (ok) => {
      if (!ok) return
      setAddress('')
      await onChanged()
    })
  }

  function remove(a: string) {
    void action.run(() => removeRequesterAddress(r.id, a)).then(async (ok) => { if (ok) await onChanged() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('requester.addresses')}</h2>
      <p className="hint">{t('requester.addressesHint')}</p>
      {r.addresses.length === 0 && <p className="empty">{t('requester.noAddresses')}</p>}
      <ul className="chips">
        {r.addresses.map((a) => (
          <li key={a}>
            {a}
            {/* A requester keeps at least one address. */}
            {!readOnly && r.addresses.length > 1 && (
              <button type="button" className="link danger" disabled={action.busy}
                aria-label={t('requester.removeAddress', { address: a })} onClick={() => remove(a)}>
                {t('action.remove')}
              </button>
            )}
          </li>
        ))}
      </ul>
      {!readOnly && (
        <form className="inline-form" onSubmit={add}>
          <label>
            {t('requester.newAddress')}
            <input type="email" required value={address} onChange={(e) => setAddress(e.target.value)} />
          </label>
          <button type="submit" disabled={action.busy || !address.trim()}>{t('requester.addAddress')}</button>
        </form>
      )}
      <Problem error={action.error} />
    </section>
  )
}

/** Marking the requester departed, and bringing them back. */
function Departure({ requester: r, onChanged, onSignedOut }: Omit<PartProps, 'readOnly'>) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const id = useId()
  const name = requesterLabel(t, r.name, r.addresses[0])

  function depart() {
    if (!window.confirm(t('requester.confirmDepart', { name }))) return
    void action.run(() => departRequester(r.id)).then(async (ok) => { if (ok) await onChanged() })
  }

  function bringBack() {
    void action.run(() => bringBackRequester(r.id)).then(async (ok) => { if (ok) await onChanged() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('requester.departure')}</h2>
      {r.departed_at ? (
        <div className="danger-row">
          <p className="hint">
            {r.addresses.length === 0 ? t('requester.comeBackNeedsAddress') : t('requester.comeBackHint')}
          </p>
          <button type="button" disabled={action.busy || r.addresses.length === 0} onClick={bringBack}>
            {t('requester.comeBack')}
          </button>
        </div>
      ) : (
        <div className="danger-row">
          <p className="hint">{t('requester.departHint')}</p>
          <button type="button" className="danger" disabled={action.busy} onClick={depart}>
            {t('requester.depart')}
          </button>
        </div>
      )}
      <Problem error={action.error} />
    </section>
  )
}
