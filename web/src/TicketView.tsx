import { useCallback, useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from 'react'
import {
  addNote,
  getTicket,
  listAgents,
  listCustomers,
  listCustomerServices,
  moveTicket,
  reply,
  setAssignee,
  setState,
  Unauthorized,
  type AgentEntry,
  type CustomerEntry,
  type Moved,
  type Service,
  type TicketDetail,
  type TicketState,
  type TimelineEntry,
} from './api/client'
import { useI18n } from './i18n/context'
import { TicketAssets, TicketFamily, TicketServices } from './TicketLinks'
import { useLoad } from './useAction'

const states: TicketState[] = ['open', 'waiting', 'resolved', 'closed']

interface Props {
  number: number
  /** Where "back" goes: the ticket list as it was last filtered. */
  backHref: string
  /** Called after a change, so the lists follow it. */
  onChanged: () => void
  onSignedOut: () => void
}

export function TicketView({ number, backHref, onChanged, onSignedOut }: Props) {
  const { t, formatTime, errorText } = useI18n()
  const [ticket, setTicket] = useState<TicketDetail | null>(null)
  // Failures are kept as they came and put into words when shown, so they follow the language.
  const [loadError, setLoadError] = useState<unknown>(null)
  const [agents, setAgents] = useState<AgentEntry[]>([])
  // Customers that can get tickets, for moving the ticket to one of them.
  const [customers, setCustomers] = useState<CustomerEntry[] | null>(null)
  const [actionError, setActionError] = useState<unknown>(null)
  const [busy, setBusy] = useState(false)
  const [moving, setMoving] = useState(false)
  // What the last move did: how many tickets it took along, how many links it removed.
  const [moved, setMoved] = useState<Moved | null>(null)
  // Only the latest load may update the view: a slow poll must not overwrite a newer answer.
  const latest = useRef(0)

  const fail = useCallback((error: unknown, show: (error: unknown) => void) => {
    if (error instanceof Unauthorized) onSignedOut()
    else show(error)
  }, [onSignedOut])

  const load = useCallback(async () => {
    const mine = ++latest.current
    try {
      const detail = await getTicket(number)
      if (mine !== latest.current) return
      setTicket(detail)
      setLoadError(null)
    } catch (error) {
      if (mine === latest.current) fail(error, setLoadError)
    }
  }, [number, fail])

  useEffect(() => {
    // Polled like the lists, so that a queued reply shows when it leaves.
    const first = setTimeout(load, 0)
    const timer = setInterval(load, 10_000)
    return () => {
      clearTimeout(first)
      clearInterval(timer)
    }
  }, [load])

  useEffect(() => {
    listAgents().then(setAgents, (error) => fail(error, setActionError))
    listCustomers().then(setCustomers, (error) => fail(error, setActionError))
  }, [fail])

  /** After a change made by one of the parts below: reloads the ticket and the lists. */
  const changed = useCallback(async () => {
    await load()
    onChanged()
  }, [load, onChanged])

  /** Runs a change, then reloads the ticket and the lists. Returns whether it succeeded. */
  async function change(action: () => Promise<void>): Promise<boolean> {
    setBusy(true)
    setActionError(null)
    try {
      await action()
      return true
    } catch (error) {
      fail(error, setActionError)
      return false
    } finally {
      setBusy(false)
      await load()
      onChanged()
    }
  }

  function changeState(state: TicketState) {
    if (state === 'closed' && !window.confirm(t('ticket.confirmClose', { number }))) return
    void change(() => setState(number, state))
  }

  if (!ticket) {
    return (
      <div className="ticket">
        <a className="back" href={backHref}>{t('ticket.back')}</a>
        {loadError !== null && <p className="error" role="alert">{errorText(loadError)}</p>}
      </div>
    )
  }

  const closed = ticket.state === 'closed'
  const requester = ticket.requester
  return (
    <div className="ticket">
      <a className="back" href={backHref}>{t('ticket.back')}</a>
      <h1><span className="number">#{ticket.number}</span> {ticket.title}</h1>
      {loadError !== null && <p className="error" role="alert">{errorText(loadError)}</p>}

      <dl className="facts">
        <div>
          <dt>{t('ticket.customer')}</dt>
          <dd>
            {ticket.customer.name}{' '}
            {!closed && !moving && (
              <button type="button" className="link" disabled={busy} onClick={() => setMoving(true)}>
                {t('ticket.changeCustomer')}
              </button>
            )}
          </dd>
        </div>
        <div>
          <dt>{t('ticket.requester')}</dt>
          <dd>
            {requester
              ? requester.name ? <>{requester.name} <span className="muted">&lt;{requester.email}&gt;</span></> : requester.email
              : <span className="muted">{t('ticket.noRequester')}</span>}
          </dd>
        </div>
        <div>
          <dt><label htmlFor="ticket-state">{t('ticket.state')}</label></dt>
          <dd>
            <select id="ticket-state" className={`state ${ticket.state}`} value={ticket.state}
              disabled={closed || busy} onChange={(e) => changeState(e.target.value as TicketState)}>
              {states.map((s) => <option key={s} value={s}>{t(`state.${s}`)}</option>)}
            </select>
          </dd>
        </div>
        <div>
          <dt><label htmlFor="ticket-assignee">{t('ticket.assignee')}</label></dt>
          <dd>
            <select id="ticket-assignee" value={ticket.assignee?.id ?? ''} disabled={closed || busy}
              onChange={(e) => void change(() => setAssignee(number, e.target.value || null))}>
              <option value="">{t('ticket.unassigned')}</option>
              {/* The current assignee stays visible even before the agents list arrives. */}
              {ticket.assignee && !agents.some((a) => a.id === ticket.assignee?.id) && (
                <option value={ticket.assignee.id}>{ticket.assignee.name}</option>
              )}
              {agents.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
            </select>
          </dd>
        </div>
        <div><dt>{t('ticket.opened')}</dt><dd className="when">{formatTime(ticket.created_at)}</dd></div>
      </dl>
      {moving && (
        <MoveCustomer ticket={ticket} customers={customers} busy={busy} onCancel={() => setMoving(false)}
          onSignedOut={onSignedOut}
          onMove={async (customerId, requesterMove) => {
            let result: Moved | null = null
            const ok = await change(async () => {
              result = await moveTicket(number, customerId, requesterMove)
            })
            if (ok) {
              setMoving(false)
              setMoved(result)
            }
          }} />
      )}
      {moved !== null && (
        <p className="done" role="status">
          {moved.moved > 1 ? t('ticket.movedWithRequester', { count: moved.moved }) : t('ticket.moved')}
          {moved.unlinked > 0 && <> {t('ticket.unlinked', { count: moved.unlinked })}</>}
        </p>
      )}
      {actionError !== null && <p className="error" role="alert">{errorText(actionError)}</p>}

      <div className="ticket-links">
        <TicketAssets ticket={ticket} onChanged={changed} onSignedOut={onSignedOut} />
        <TicketServices ticket={ticket} onChanged={changed} onSignedOut={onSignedOut} />
        <TicketFamily ticket={ticket} onChanged={changed} onSignedOut={onSignedOut} />
      </div>

      <ol className="timeline">
        {ticket.timeline.map((entry) => <Entry key={`${entry.kind}-${entry.id}`} entry={entry} />)}
        {ticket.timeline.length === 0 && <li className="empty">{t('ticket.emptyTimeline')}</li>}
      </ol>

      {closed && (
        <p className="closed-notice">{t('ticket.closedNotice')}</p>
      )}
      <Composer ticket={ticket} onDone={() => { void load(); onChanged() }} onSignedOut={onSignedOut} />
    </div>
  )
}

function Entry({ entry }: { entry: TimelineEntry }) {
  const { t, formatTime } = useI18n()
  if (entry.kind === 'note') {
    return (
      <li className="entry note">
        <div className="entry-head">
          <span className="tag note-tag">{t('entry.note')}</span>
          <span className="who">{entry.author_name}</span>
          <time className="when" dateTime={entry.created_at}>{formatTime(entry.created_at)}</time>
        </div>
        <p className="body">{entry.body}</p>
      </li>
    )
  }
  const outbound = entry.direction === 'outbound'
  const sender = entry.from_name ? `${entry.from_name} <${entry.from_address}>` : entry.from_address
  return (
    <li className={`entry message ${outbound ? 'outbound' : 'inbound'}`}>
      <div className="entry-head">
        <span className="tag">{outbound ? t('entry.outgoing') : t('entry.incoming')}</span>
        <span className="who">
          {outbound ? <>{entry.author_name ?? sender} <span className="muted">{t('entry.to', { address: entry.to_address ?? '—' })}</span></> : sender}
        </span>
        {outbound && (entry.sent_at
          ? <span className="status sent">{t('entry.sent', { time: formatTime(entry.sent_at) })}</span>
          : entry.gave_up
            ? <span className="status failed">{t('entry.notSent')}</span>
            : <span className="status queued">{t('entry.queued')}</span>)}
        <time className="when" dateTime={entry.created_at}>{formatTime(entry.created_at)}</time>
      </div>
      {entry.subject && <div className="subject">{entry.subject}</div>}
      <p className="body">{entry.body_text}</p>
      {outbound && !entry.sent_at && entry.gave_up && (
        <p className="error" role="alert">
          {t('entry.gaveUp', { error: entry.last_error ?? t('entry.unknownError') })}
        </p>
      )}
      {outbound && !entry.sent_at && !entry.gave_up && entry.last_error && (
        <p className="error">{t('entry.retrying', { error: entry.last_error })}</p>
      )}
    </li>
  )
}

type Mode = 'reply' | 'note'
const strong = (inner: ReactNode) => <strong>{inner}</strong>
// Closed is left out on purpose: closing is final and goes through the state control and its
// confirmation.
const thenStates: TicketState[] = ['open', 'waiting', 'resolved']

function Composer({ ticket, onDone, onSignedOut }: {
  ticket: TicketDetail
  onDone: () => void
  onSignedOut: () => void
}) {
  const { t, rich, errorText } = useI18n()
  const requester = ticket.requester
  // Chosen once per ticket; the poll never changes it.
  const [chosen, setChosen] = useState<Mode>(requester ? 'reply' : 'note')
  const [body, setBody] = useState('')
  const [then, setThen] = useState<TicketState>('waiting')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const id = useId()
  // Without a requester there is nobody to reply to, and a closed ticket gets no more replies.
  const closed = ticket.state === 'closed'
  const canReply = requester !== null && requester !== undefined && !closed
  const mode: Mode = canReply ? chosen : 'note'

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (busy || !body.trim()) return
    setBusy(true)
    setError(null)
    try {
      if (mode === 'reply') await reply(ticket.number, body, then)
      else await addNote(ticket.number, body)
      setBody('')
      setThen('waiting')
      onDone()
    } catch (e) {
      if (e instanceof Unauthorized) onSignedOut()
      else setError(e)
    } finally {
      setBusy(false)
    }
  }

  return (
    <form className={`composer ${mode}`} onSubmit={submit}>
      <fieldset className="modes">
        <legend className="visually-hidden">{t('composer.modes')}</legend>
        <label className={mode === 'reply' ? 'current' : ''}>
          <input type="radio" name={`${id}-mode`} value="reply" checked={mode === 'reply'}
            disabled={!canReply} onChange={() => setChosen('reply')} />
          {t('composer.reply')}
        </label>
        <label className={mode === 'note' ? 'current' : ''}>
          <input type="radio" name={`${id}-mode`} value="note" checked={mode === 'note'}
            onChange={() => setChosen('note')} />
          {t('composer.note')}
        </label>
      </fieldset>
      <p className="mode-hint" id={`${id}-hint`}>
        {mode === 'reply' && requester
          ? rich('composer.hintReply', { email: requester.email, strong })
          : closed
            ? t('composer.hintClosed')
            : requester
              ? rich('composer.hintNote', { strong })
              : t('composer.hintNoRequester')}
      </p>
      <label className="visually-hidden" htmlFor={`${id}-body`}>
        {mode === 'reply' ? t('composer.replyLabel') : t('composer.noteLabel')}
      </label>
      <textarea id={`${id}-body`} rows={5} value={body} aria-describedby={`${id}-hint`}
        placeholder={mode === 'reply' ? t('composer.replyPlaceholder') : t('composer.notePlaceholder')}
        onChange={(e) => setBody(e.target.value)} />
      {error !== null && <p className="error" role="alert">{errorText(error)}</p>}
      <div className="composer-foot">
        {mode === 'reply' && (
          <label>
            {t('composer.then')}{' '}
            <select value={then} onChange={(e) => setThen(e.target.value as TicketState)}>
              {thenStates.map((s) => <option key={s} value={s}>{t(`state.${s}`)}</option>)}
            </select>
          </label>
        )}
        <button type="submit" disabled={busy || !body.trim()}>
          {mode === 'reply' && requester ? t('composer.sendReply', { email: requester.email }) : t('composer.addNote')}
        </button>
      </div>
    </form>
  )
}

/** Moves the ticket to the right customer, and says what happens to the requester. */
function MoveCustomer({ ticket, customers, busy, onCancel, onMove, onSignedOut }: {
  ticket: TicketDetail
  customers: CustomerEntry[] | null
  busy: boolean
  onCancel: () => void
  onMove: (customerId: string, requester: 'move' | 'remove') => Promise<void>
  onSignedOut: () => void
}) {
  const { t, errorText } = useI18n()
  const [customerId, setCustomerId] = useState('')
  const [requesterMove, setRequesterMove] = useState<'move' | 'remove'>('move')
  const id = useId()
  const requester = ticket.requester
  const others = requester ? requester.ticket_count - 1 : 0
  const linked = ticket.assets.length + ticket.services.length > 0
  // What the chosen customer uses decides which linked services stay.
  const loadUsed = useCallback(
    (): Promise<Service[]> => (linked && customerId ? listCustomerServices(customerId) : Promise.resolve([])),
    [linked, customerId])
  const used = useLoad(loadUsed, onSignedOut)
  const target = customers?.find((c) => c.id === customerId)
  const dropped = target && used.data
    ? ticket.services.filter((s) => !used.data?.some((u) => u.id === s.id))
    : null
  const error = used.error

  function submit(event: FormEvent) {
    event.preventDefault()
    if (customerId) void onMove(customerId, requesterMove)
  }

  return (
    <form className="move" onSubmit={submit}>
      <label htmlFor={`${id}-customer`}>{t('move.to')}</label>
      <select id={`${id}-customer`} value={customerId} onChange={(e) => setCustomerId(e.target.value)}>
        <option value="">{t('customer.choose')}</option>
        {customers?.filter((c) => c.id !== ticket.customer.id).map((c) => (
          <option key={c.id} value={c.id}>{c.is_help_desk ? t('customer.helpDesk', { name: c.name }) : c.name}</option>
        ))}
      </select>
      {requester && (
        <fieldset>
          <legend>{t('move.requesterBelongs', { email: requester.email, customer: ticket.customer.name })}</legend>
          <label>
            <input type="radio" name={`${id}-requester`} checked={requesterMove === 'move'}
              onChange={() => setRequesterMove('move')} />
            {others > 0
              ? t('move.requesterTooWithTickets', { count: others })
              : t('move.requesterToo')}
          </label>
          <label>
            <input type="radio" name={`${id}-requester`} checked={requesterMove === 'remove'}
              onChange={() => setRequesterMove('remove')} />
            {t('move.requesterStays', { customer: ticket.customer.name })}
          </label>
        </fieldset>
      )}
      {linked && (
        <div className="warning" role="note">
          {!target || !dropped ? (
            <p>{t('move.linksWarning', { customer: ticket.customer.name })}</p>
          ) : ticket.assets.length + dropped.length === 0 ? (
            <p>{t('move.keepsLinks', { customer: target.name })}</p>
          ) : (
            <>
              <p>{t('move.willUnlink')}</p>
              <ul>
                {ticket.assets.map((a) => (
                  <li key={a.id}>{t('move.unlinkAsset', { name: a.name, kind: a.kind, customer: ticket.customer.name })}</li>
                ))}
                {dropped.map((s) => (
                  <li key={s.id}>{t('move.unlinkService', { name: s.name, customer: target.name })}</li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
      {error !== null && <p className="error" role="alert">{errorText(error)}</p>}
      <div className="move-foot">
        <button type="button" onClick={onCancel}>{t('move.cancel')}</button>
        <button type="submit" disabled={busy || !customerId}>{t('move.submit')}</button>
      </div>
    </form>
  )
}
