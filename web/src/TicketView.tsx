import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from 'react'
import {
  addNote,
  getTicket,
  listAgents,
  reply,
  setAssignee,
  setState,
  Unauthorized,
  type AgentEntry,
  type TicketDetail,
  type TicketState,
  type TimelineEntry,
} from './api/client'
import { errorText, formatTime, stateLabels } from './format'
import { ticketsHref } from './route'

const states: TicketState[] = ['open', 'waiting', 'resolved', 'closed']

interface Props {
  number: number
  /** Called after a change, so the lists follow it. */
  onChanged: () => void
  onSignedOut: () => void
}

export function TicketView({ number, onChanged, onSignedOut }: Props) {
  const [ticket, setTicket] = useState<TicketDetail | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [agents, setAgents] = useState<AgentEntry[]>([])
  const [actionError, setActionError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  // Only the latest load may update the view: a slow poll must not overwrite a newer answer.
  const latest = useRef(0)

  const fail = useCallback((error: unknown, show: (text: string) => void) => {
    if (error instanceof Unauthorized) onSignedOut()
    else show(errorText(error))
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
  }, [fail])

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
    if (state === 'closed' && !window.confirm(
      `Close ticket #${number}? Closing is final: the ticket cannot be reopened, and a new message ` +
      'from the requester will open a new ticket.')) return
    void change(() => setState(number, state))
  }

  if (!ticket) {
    return (
      <div className="ticket">
        <a className="back" href={ticketsHref}>← Tickets</a>
        {loadError && <p className="error" role="alert">{loadError}</p>}
      </div>
    )
  }

  const closed = ticket.state === 'closed'
  const requester = ticket.requester
  return (
    <div className="ticket">
      <a className="back" href={ticketsHref}>← Tickets</a>
      <h1><span className="number">#{ticket.number}</span> {ticket.title}</h1>
      {loadError && <p className="error" role="alert">{loadError}</p>}

      <dl className="facts">
        <div><dt>Customer</dt><dd>{ticket.customer.name}</dd></div>
        <div>
          <dt>Requester</dt>
          <dd>
            {requester
              ? requester.name ? <>{requester.name} <span className="muted">&lt;{requester.email}&gt;</span></> : requester.email
              : <span className="muted">None</span>}
          </dd>
        </div>
        <div>
          <dt><label htmlFor="ticket-state">State</label></dt>
          <dd>
            <select id="ticket-state" className={`state ${ticket.state}`} value={ticket.state}
              disabled={closed || busy} onChange={(e) => changeState(e.target.value as TicketState)}>
              {states.map((s) => <option key={s} value={s}>{stateLabels[s]}</option>)}
            </select>
          </dd>
        </div>
        <div>
          <dt><label htmlFor="ticket-assignee">Assignee</label></dt>
          <dd>
            <select id="ticket-assignee" value={ticket.assignee?.id ?? ''} disabled={closed || busy}
              onChange={(e) => void change(() => setAssignee(number, e.target.value || null))}>
              <option value="">Unassigned</option>
              {/* The current assignee stays visible even before the agents list arrives. */}
              {ticket.assignee && !agents.some((a) => a.id === ticket.assignee?.id) && (
                <option value={ticket.assignee.id}>{ticket.assignee.name}</option>
              )}
              {agents.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
            </select>
          </dd>
        </div>
        <div><dt>Opened</dt><dd className="when">{formatTime(ticket.created_at)}</dd></div>
      </dl>
      {actionError && <p className="error" role="alert">{actionError}</p>}

      <ol className="timeline">
        {ticket.timeline.map((entry) => <Entry key={`${entry.kind}-${entry.id}`} entry={entry} />)}
        {ticket.timeline.length === 0 && <li className="empty">Nothing written on this ticket yet.</li>}
      </ol>

      {closed ? (
        <p className="closed-notice">
          This ticket is closed, which is final. A new message from the requester opens a new ticket.
        </p>
      ) : (
        <Composer ticket={ticket} onDone={() => { void load(); onChanged() }} onSignedOut={onSignedOut} />
      )}
    </div>
  )
}

function Entry({ entry }: { entry: TimelineEntry }) {
  if (entry.kind === 'note') {
    return (
      <li className="entry note">
        <div className="entry-head">
          <span className="tag note-tag">Internal note</span>
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
        <span className="tag">{outbound ? 'Outgoing message' : 'Incoming message'}</span>
        <span className="who">
          {outbound ? <>{entry.author_name ?? sender} <span className="muted">to {entry.to_address ?? '—'}</span></> : sender}
        </span>
        {outbound && (entry.sent_at
          ? <span className="status sent">Sent {formatTime(entry.sent_at)}</span>
          : <span className="status queued">Queued</span>)}
        <time className="when" dateTime={entry.created_at}>{formatTime(entry.created_at)}</time>
      </div>
      {entry.subject && <div className="subject">{entry.subject}</div>}
      <p className="body">{entry.body_text}</p>
      {outbound && !entry.sent_at && entry.last_error && (
        <p className="error">Not sent yet: {entry.last_error}</p>
      )}
    </li>
  )
}

type Mode = 'reply' | 'note'
// Closed is left out on purpose: closing is final and goes through the state control and its
// confirmation.
const thenStates: TicketState[] = ['open', 'waiting', 'resolved']

function Composer({ ticket, onDone, onSignedOut }: {
  ticket: TicketDetail
  onDone: () => void
  onSignedOut: () => void
}) {
  const requester = ticket.requester
  // Chosen once per ticket; the poll never changes it.
  const [chosen, setChosen] = useState<Mode>(requester ? 'reply' : 'note')
  const [body, setBody] = useState('')
  const [then, setThen] = useState<TicketState>('waiting')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const id = useId()
  // Without a requester there is nobody to reply to.
  const mode: Mode = requester ? chosen : 'note'

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
      else setError(errorText(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <form className={`composer ${mode}`} onSubmit={submit}>
      <fieldset className="modes">
        <legend className="visually-hidden">What you are writing</legend>
        <label className={mode === 'reply' ? 'current' : ''}>
          <input type="radio" name={`${id}-mode`} value="reply" checked={mode === 'reply'}
            disabled={!requester} onChange={() => setChosen('reply')} />
          Reply to requester
        </label>
        <label className={mode === 'note' ? 'current' : ''}>
          <input type="radio" name={`${id}-mode`} value="note" checked={mode === 'note'}
            onChange={() => setChosen('note')} />
          Internal note
        </label>
      </fieldset>
      <p className="mode-hint" id={`${id}-hint`}>
        {mode === 'reply' && requester
          ? <>Sent by e-mail to <strong>{requester.email}</strong>.</>
          : requester
            ? <>Only agents see a note. It is <strong>never sent</strong> to the requester.</>
            : <>This ticket has no requester, so there is nobody to reply to. Only agents see a note.</>}
      </p>
      <label className="visually-hidden" htmlFor={`${id}-body`}>
        {mode === 'reply' ? 'Reply to the requester' : 'Internal note'}
      </label>
      <textarea id={`${id}-body`} rows={5} value={body} aria-describedby={`${id}-hint`}
        placeholder={mode === 'reply' ? 'Write to the requester…' : 'Write a note for the agents…'}
        onChange={(e) => setBody(e.target.value)} />
      {error && <p className="error" role="alert">{error}</p>}
      <div className="composer-foot">
        {mode === 'reply' && (
          <label>
            Then set the ticket to{' '}
            <select value={then} onChange={(e) => setThen(e.target.value as TicketState)}>
              {thenStates.map((s) => <option key={s} value={s}>{stateLabels[s]}</option>)}
            </select>
          </label>
        )}
        <button type="submit" disabled={busy || !body.trim()}>
          {mode === 'reply' && requester ? `Send reply to ${requester.email}` : 'Add internal note'}
        </button>
      </div>
    </form>
  )
}
