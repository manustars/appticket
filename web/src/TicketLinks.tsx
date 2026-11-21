// What a ticket refers to (assets of its customer, services its customer uses) and how it relates
// to other tickets (its parent, or its children).
import { useCallback, useId, useState, type FormEvent } from 'react'
import {
  clearParent,
  linkTicketAsset,
  linkTicketService,
  listCustomerAssets,
  listCustomerServices,
  resolveChildren,
  setParent,
  unlinkTicketAsset,
  unlinkTicketService,
  type Asset,
  type ResolvedChildren,
  type Service,
  type TicketDetail,
} from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { ticketHref } from './route'
import { useAction, useLoad } from './useAction'

interface Props {
  ticket: TicketDetail
  /** Called after a change: reloads the ticket and the lists. */
  onChanged: () => Promise<void>
  onSignedOut: () => void
}

/** Nothing to choose from: the links of a closed ticket do not change. */
const nothing = () => Promise.resolve([])

export function TicketAssets({ ticket, onChanged, onSignedOut }: Props) {
  const { t } = useI18n()
  const closed = ticket.state === 'closed'
  const customerId = ticket.customer.id
  // Reloaded when the ticket moves to another customer.
  const load = useCallback(
    (): Promise<Asset[]> => (closed ? nothing() : listCustomerAssets(customerId)), [closed, customerId])
  const options = useLoad(load, onSignedOut)
  const action = useAction(onSignedOut)
  const [chosen, setChosen] = useState('')
  const id = useId()
  // Retired assets get no new links.
  const candidates = options.data?.filter((a) => !a.retired_at && !ticket.assets.some((l) => l.id === a.id)) ?? []

  function link(event: FormEvent) {
    event.preventDefault()
    if (!chosen) return
    void action.run(() => linkTicketAsset(ticket.number, chosen)).then(async (ok) => {
      if (ok) setChosen('')
      await onChanged()
    })
  }

  function unlink(assetId: string) {
    void action.run(() => unlinkTicketAsset(ticket.number, assetId)).then(() => onChanged())
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('links.assets')}</h2>
      {ticket.assets.length === 0 ? (
        <p className="empty">{t('links.noAssets')}</p>
      ) : (
        <ul className="items">
          {ticket.assets.map((a) => (
            <li key={a.id}>
              {a.name} <span className="muted">{a.kind}</span>
              {!closed && (
                <button type="button" className="link danger" disabled={action.busy}
                  aria-label={t('links.unlinkNamed', { name: a.name })} onClick={() => unlink(a.id)}>
                  {t('action.unlink')}
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {!closed && (
        <form className="inline-form" onSubmit={link}>
          <label>
            {t('links.addAsset', { customer: ticket.customer.name })}
            <select value={chosen} disabled={!options.data || candidates.length === 0}
              onChange={(e) => setChosen(e.target.value)}>
              <option value="">
                {!options.data ? t('links.loading')
                  : candidates.length === 0 ? t('links.noAssetLeft') : t('asset.choose')}
              </option>
              {candidates.map((a) => <option key={a.id} value={a.id}>{a.name} ({a.kind})</option>)}
            </select>
          </label>
          <button type="submit" disabled={action.busy || !chosen}>{t('action.link')}</button>
        </form>
      )}
      <Problem error={options.error ?? action.error} />
    </section>
  )
}

export function TicketServices({ ticket, onChanged, onSignedOut }: Props) {
  const { t } = useI18n()
  const closed = ticket.state === 'closed'
  const customerId = ticket.customer.id
  const load = useCallback(
    (): Promise<Service[]> => (closed ? nothing() : listCustomerServices(customerId)), [closed, customerId])
  const options = useLoad(load, onSignedOut)
  const action = useAction(onSignedOut)
  const [chosen, setChosen] = useState('')
  const id = useId()
  const candidates = options.data?.filter((s) => !s.retired_at && !ticket.services.some((l) => l.id === s.id)) ?? []

  function link(event: FormEvent) {
    event.preventDefault()
    if (!chosen) return
    void action.run(() => linkTicketService(ticket.number, chosen)).then(async (ok) => {
      if (ok) setChosen('')
      await onChanged()
    })
  }

  function unlink(serviceId: string) {
    void action.run(() => unlinkTicketService(ticket.number, serviceId)).then(() => onChanged())
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('links.services')}</h2>
      {ticket.services.length === 0 ? (
        <p className="empty">{t('links.noServices')}</p>
      ) : (
        <ul className="items">
          {ticket.services.map((s) => (
            <li key={s.id}>
              {s.name}
              {!closed && (
                <button type="button" className="link danger" disabled={action.busy}
                  aria-label={t('links.unlinkNamed', { name: s.name })} onClick={() => unlink(s.id)}>
                  {t('action.unlink')}
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {!closed && (
        <form className="inline-form" onSubmit={link}>
          <label>
            {t('links.addService', { customer: ticket.customer.name })}
            <select value={chosen} disabled={!options.data || candidates.length === 0}
              onChange={(e) => setChosen(e.target.value)}>
              <option value="">
                {!options.data ? t('links.loading')
                  : candidates.length === 0 ? t('links.noServiceLeft') : t('service.choose')}
              </option>
              {candidates.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
            </select>
          </label>
          <button type="submit" disabled={action.busy || !chosen}>{t('action.link')}</button>
        </form>
      )}
      <Problem error={options.error ?? action.error} />
    </section>
  )
}

/**
 * How the ticket relates to others. Any ticket without children can have a parent, the help desk's
 * own included (its colleagues can be hit by the same failure); a parent cannot become a child. A
 * help desk ticket that is not a child can become a parent, so it also shows its (empty) children.
 */
export function TicketFamily({ ticket, onChanged, onSignedOut }: Props) {
  const parent = ticket.children.length === 0
  const children = ticket.children.length > 0 || (ticket.customer.is_help_desk && !ticket.parent)
  return (
    <>
      {parent && <Parent ticket={ticket} onChanged={onChanged} onSignedOut={onSignedOut} />}
      {children && <Children ticket={ticket} onChanged={onChanged} onSignedOut={onSignedOut} />}
    </>
  )
}

function Parent({ ticket, onChanged, onSignedOut }: Props) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [number, setNumber] = useState('')
  const [invalid, setInvalid] = useState(false)
  const id = useId()
  // Closed tickets are neither attached nor detached.
  const closed = ticket.state === 'closed'
  const parent = ticket.parent

  function attach(event: FormEvent) {
    event.preventDefault()
    const n = Number(number)
    if (!Number.isInteger(n) || n < 1) {
      setInvalid(true)
      return
    }
    setInvalid(false)
    void action.run(() => setParent(ticket.number, n)).then(async (ok) => {
      if (ok) setNumber('')
      await onChanged()
    })
  }

  function detach() {
    void action.run(() => clearParent(ticket.number)).then(() => onChanged())
  }

  return (
    <section className="panel family" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('parent.title')}</h2>
      {parent ? (
        <ul className="items">
          <li>
            <RelatedLink ticket={parent} />
            {!closed && (
              <button type="button" className="link danger" disabled={action.busy} onClick={detach}>
                {t('parent.detach')}
              </button>
            )}
          </li>
        </ul>
      ) : (
        <p className="empty">{t('parent.none')}</p>
      )}
      {!parent && !closed && (
        <>
          <form className="inline-form" onSubmit={attach}>
            <label>
              {t('parent.number')}
              <input type="number" min={1} step={1} inputMode="numeric" required value={number}
                aria-describedby={`${id}-hint`} onChange={(e) => setNumber(e.target.value)} />
            </label>
            <button type="submit" disabled={action.busy || !number}>{t('parent.attach')}</button>
          </form>
          <p className="hint" id={`${id}-hint`}>{t('parent.attachHint')}</p>
        </>
      )}
      {invalid ? <p className="error" role="alert">{t('inbox.notATicket')}</p> : <Problem error={action.error} />}
    </section>
  )
}

function RelatedLink({ ticket }: { ticket: TicketDetail['children'][number] }) {
  const { t } = useI18n()
  return (
    <span className="related">
      <a href={ticketHref(ticket.number)}><span className="number">#{ticket.number}</span> {ticket.title}</a>
      <span className="muted">{ticket.customer_name}</span>
      <span className={`state ${ticket.state}`}>{t(`state.${ticket.state}`)}</span>
    </span>
  )
}

function Children({ ticket, onChanged, onSignedOut }: Props) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [open, setOpen] = useState(false)
  const [body, setBody] = useState('')
  // What the last resolve did, kept after the children it resolved have changed state.
  const [done, setDone] = useState<ResolvedChildren | null>(null)
  const id = useId()
  const pending = ticket.children.filter((c) => c.state === 'open' || c.state === 'waiting').length

  function submit(event: FormEvent) {
    event.preventDefault()
    if (!body.trim()) return
    if (!window.confirm(t('parent.confirmResolve', { count: pending, number: ticket.number }))) return
    let result: ResolvedChildren | null = null
    void action.run(async () => {
      result = await resolveChildren(ticket.number, body)
    }).then(async (ok) => {
      if (ok) {
        setDone(result)
        setBody('')
        setOpen(false)
      }
      await onChanged()
    })
  }

  return (
    <section className="panel family" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('parent.children')}</h2>
      {ticket.children.length === 0 ? (
        <p className="empty">{t('parent.noChildren')}</p>
      ) : (
        <ul className="items">
          {ticket.children.map((c) => <li key={c.number}><RelatedLink ticket={c} /></li>)}
        </ul>
      )}
      {done && (
        <p className="done" role="status">
          {t('parent.resolved', { count: done.resolved })} {t('parent.replied', { count: done.replied })}
        </p>
      )}
      {pending > 0 && !open && (
        <div>
          <button type="button" onClick={() => { setOpen(true); setDone(null) }}>{t('parent.resolveChildren')}</button>
        </div>
      )}
      {pending > 0 && open && (
        <form className="resolve" onSubmit={submit}>
          <label htmlFor={`${id}-body`}>{t('parent.message')}</label>
          <textarea id={`${id}-body`} rows={4} value={body} aria-describedby={`${id}-hint`}
            onChange={(e) => setBody(e.target.value)} />
          <p className="hint" id={`${id}-hint`}>{t('parent.resolveHint', { count: pending })}</p>
          <div className="move-foot">
            <button type="button" onClick={() => { setOpen(false); action.clear() }}>{t('action.cancel')}</button>
            <button type="submit" disabled={action.busy || !body.trim()}>{t('parent.resolveChildren')}</button>
          </div>
        </form>
      )}
      <Problem error={action.error} />
    </section>
  )
}
