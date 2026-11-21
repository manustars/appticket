import { useEffect, useId, useState, type FormEvent } from 'react'
import {
  attachInboxMessage,
  discardInboxMessage,
  listCustomers,
  openTicketFromInbox,
  Unauthorized,
  type CustomerEntry,
  type InboxMessage,
} from './api/client'
import { errorText, formatTime } from './format'
import { ticketHref } from './route'

interface Props {
  messages: InboxMessage[] | null
  /** Called after an action, so the lists follow it. */
  onChanged: () => void
  onSignedOut: () => void
}

export function Inbox({ messages, onChanged, onSignedOut }: Props) {
  const [customers, setCustomers] = useState<CustomerEntry[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  // What the last action did, kept after its message has left the inbox.
  const [done, setDone] = useState<{ text: string; ticket?: number } | null>(null)

  useEffect(() => {
    listCustomers().then(setCustomers, (e) => {
      if (e instanceof Unauthorized) onSignedOut()
      else setError(errorText(e))
    })
  }, [onSignedOut])

  if (messages === null) return null
  return (
    <>
      {error && <p className="error" role="alert">{error}</p>}
      {done && (
        <p className="done" role="status">
          {done.text}{' '}
          {done.ticket !== undefined && <a href={ticketHref(done.ticket)}>Go to ticket #{done.ticket}</a>}
        </p>
      )}
      {messages.length === 0 ? (
        <p className="empty">The inbox is empty.</p>
      ) : (
        <ul className="inbox">
          {messages.map((m) => (
            <InboxItem key={m.id} message={m} customers={customers} onSignedOut={onSignedOut}
              onDone={(result) => { setDone(result); onChanged() }} />
          ))}
        </ul>
      )}
    </>
  )
}

type Action = 'open' | 'attach' | null

function InboxItem({ message: m, customers, onDone, onSignedOut }: {
  message: InboxMessage
  customers: CustomerEntry[] | null
  onDone: (result: { text: string; ticket?: number }) => void
  onSignedOut: () => void
}) {
  const [action, setAction] = useState<Action>(null)
  const [customerId, setCustomerId] = useState('')
  const [ticketNumber, setTicketNumber] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const id = useId()
  const sender = m.from_name ? `${m.from_name} <${m.from_address}>` : m.from_address

  async function run(work: () => Promise<{ text: string; ticket?: number }>) {
    setBusy(true)
    setError(null)
    try {
      onDone(await work())
    } catch (e) {
      if (e instanceof Unauthorized) onSignedOut()
      else setError(errorText(e))
    } finally {
      setBusy(false)
    }
  }

  function open(event: FormEvent) {
    event.preventDefault()
    if (!customerId) return
    void run(async () => {
      const number = await openTicketFromInbox(m.id, customerId)
      return { text: `Ticket #${number} opened from “${m.subject || '(no subject)'}”.`, ticket: number }
    })
  }

  function attach(event: FormEvent) {
    event.preventDefault()
    const number = Number(ticketNumber)
    if (!Number.isInteger(number) || number < 1) {
      setError('Type the number of an existing ticket.')
      return
    }
    void run(async () => {
      await attachInboxMessage(m.id, number)
      return { text: `Message attached to ticket #${number}.`, ticket: number }
    })
  }

  function discard() {
    if (!window.confirm(`Discard the message from ${sender}? It will not become part of any ticket.`)) return
    void run(async () => {
      await discardInboxMessage(m.id)
      return { text: `Message from ${sender} discarded.` }
    })
  }

  function choose(next: Action) {
    setAction(next)
    setError(null)
  }

  return (
    <li>
      <div className="from">
        {sender}
        <span className="when">{formatTime(m.received_at)}</span>
      </div>
      <div className="subject">{m.subject || '(no subject)'}</div>
      <p className="body">{m.body_text}</p>

      <div className="actions">
        <button type="button" className={action === 'open' ? 'current' : ''} aria-expanded={action === 'open'}
          disabled={busy} onClick={() => choose(action === 'open' ? null : 'open')}>Open ticket</button>
        <button type="button" className={action === 'attach' ? 'current' : ''} aria-expanded={action === 'attach'}
          disabled={busy} onClick={() => choose(action === 'attach' ? null : 'attach')}>Attach to ticket</button>
        <button type="button" className="danger" disabled={busy} onClick={discard}>Discard</button>
      </div>

      {action === 'open' && (
        <form className="action-form" onSubmit={open}>
          <label htmlFor={`${id}-customer`}>Customer</label>
          <select id={`${id}-customer`} required value={customerId} disabled={!customers}
            onChange={(e) => setCustomerId(e.target.value)}>
            <option value="">{customers ? 'Choose a customer…' : 'Loading customers…'}</option>
            {customers?.map((c) => (
              <option key={c.id} value={c.id}>{c.is_help_desk ? `${c.name} (help desk)` : c.name}</option>
            ))}
          </select>
          <button type="submit" disabled={busy || !customerId}>Open ticket</button>
          <button type="button" className="quiet" onClick={() => choose(null)}>Cancel</button>
        </form>
      )}
      {action === 'attach' && (
        <form className="action-form" onSubmit={attach}>
          <label htmlFor={`${id}-ticket`}>Ticket number</label>
          <input id={`${id}-ticket`} type="number" min={1} step={1} required inputMode="numeric"
            value={ticketNumber} onChange={(e) => setTicketNumber(e.target.value)} />
          <button type="submit" disabled={busy || !ticketNumber}>Attach</button>
          <button type="button" className="quiet" onClick={() => choose(null)}>Cancel</button>
        </form>
      )}
      {error && <p className="error" role="alert">{error}</p>}
    </li>
  )
}
