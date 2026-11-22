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
import { useI18n } from './i18n/context'
import { InboxProposal, type TriageDone } from './Proposals'
import { ticketHref } from './route'
import { useSession } from './session'

interface Props {
  messages: InboxMessage[] | null
  /** Called after an action, so the lists follow it. */
  onChanged: () => void
  onSignedOut: () => void
}

/** What an action did; put into words when shown, so that it follows the language. */
type Done =
  | { kind: 'opened'; ticket: number; subject: string }
  | { kind: 'attached'; ticket: number }
  | { kind: 'discarded'; sender: string }

export function Inbox({ messages, onChanged, onSignedOut }: Props) {
  const { t, errorText } = useI18n()
  const [customers, setCustomers] = useState<CustomerEntry[] | null>(null)
  const [error, setError] = useState<unknown>(null)
  const canSort = useSession().can('inbox.triage')
  // What the last action did, kept after its message has left the inbox.
  const [done, setDone] = useState<Done | null>(null)

  useEffect(() => {
    listCustomers().then(setCustomers, (e) => {
      if (e instanceof Unauthorized) onSignedOut()
      else setError(e)
    })
  }, [onSignedOut])

  function doneText(done: Done): string {
    switch (done.kind) {
      case 'opened':
        return t('inbox.opened', { number: done.ticket, subject: done.subject || t('inbox.noSubject') })
      case 'attached':
        return t('inbox.attached', { number: done.ticket })
      case 'discarded':
        return t('inbox.discarded', { sender: done.sender })
    }
  }

  if (messages === null) return null
  return (
    <>
      {error !== null && <p className="error" role="alert">{errorText(error)}</p>}
      {done && (
        <p className="done" role="status">
          {doneText(done)}{' '}
          {done.kind !== 'discarded' && (
            <a href={ticketHref(done.ticket)}>{t('inbox.goToTicket', { number: done.ticket })}</a>
          )}
        </p>
      )}
      {!canSort && messages.length > 0 && <p className="notice">{t('inbox.readOnly')}</p>}
      {messages.length === 0 ? (
        <p className="empty">{t('inbox.empty')}</p>
      ) : (
        <ul className="inbox">
          {messages.map((m) => (
            <InboxItem key={m.id} message={m} customers={customers} canSort={canSort}
              onSignedOut={onSignedOut} onChanged={onChanged}
              onDone={(result) => { setDone(result); onChanged() }} />
          ))}
        </ul>
      )}
    </>
  )
}

type Action = 'open' | 'attach' | null
/** Typed into the ticket number: not a number a ticket can have. */
const notATicket = Symbol('not a ticket number')

function InboxItem({ message: m, customers, canSort, onDone, onChanged, onSignedOut }: {
  message: InboxMessage
  customers: CustomerEntry[] | null
  /** Whether the agent's role allows sorting the inbox; without it the message is only read. */
  canSort: boolean
  onDone: (result: Done) => void
  /** Something changed that leaves the message in the inbox, such as a discarded proposal. */
  onChanged: () => void
  onSignedOut: () => void
}) {
  const { t, formatTime, errorText } = useI18n()
  const [action, setAction] = useState<Action>(null)
  const [customerId, setCustomerId] = useState('')
  const [ticketNumber, setTicketNumber] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const id = useId()
  const sender = m.from_name ? `${m.from_name} <${m.from_address}>` : m.from_address

  async function run(work: () => Promise<Done>) {
    setBusy(true)
    setError(null)
    try {
      onDone(await work())
    } catch (e) {
      if (e instanceof Unauthorized) onSignedOut()
      else setError(e)
    } finally {
      setBusy(false)
    }
  }

  function open(event: FormEvent) {
    event.preventDefault()
    if (!customerId) return
    void run(async () => {
      const number = await openTicketFromInbox(m.id, customerId)
      return { kind: 'opened', ticket: number, subject: m.subject }
    })
  }

  function attach(event: FormEvent) {
    event.preventDefault()
    const number = Number(ticketNumber)
    if (!Number.isInteger(number) || number < 1) {
      setError(notATicket)
      return
    }
    void run(async () => {
      await attachInboxMessage(m.id, number)
      return { kind: 'attached', ticket: number }
    })
  }

  function discard() {
    if (!window.confirm(t('inbox.confirmDiscard', { sender }))) return
    void run(async () => {
      await discardInboxMessage(m.id)
      return { kind: 'discarded', sender }
    })
  }

  /** What an accepted proposal did, as the inbox's notice tells it. */
  function triageDone(done: TriageDone): Done {
    switch (done.kind) {
      case 'opened':
        return { kind: 'opened', ticket: done.ticket, subject: m.subject }
      case 'attached':
        return { kind: 'attached', ticket: done.ticket }
      case 'discarded':
        return { kind: 'discarded', sender }
    }
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
      <div className="subject">{m.subject || t('inbox.noSubject')}</div>
      <p className="body">{m.body_text}</p>

      {m.proposal && (
        <InboxProposal proposal={m.proposal} sender={sender} onSignedOut={onSignedOut}
          onDiscarded={onChanged}
          onDone={(done) => onDone(triageDone(done))} />
      )}

      {canSort && (
        <div className="actions">
          <button type="button" className={action === 'open' ? 'current' : ''} aria-expanded={action === 'open'}
            disabled={busy} onClick={() => choose(action === 'open' ? null : 'open')}>{t('inbox.open')}</button>
          <button type="button" className={action === 'attach' ? 'current' : ''} aria-expanded={action === 'attach'}
            disabled={busy} onClick={() => choose(action === 'attach' ? null : 'attach')}>{t('inbox.attach')}</button>
          <button type="button" className="danger" disabled={busy} onClick={discard}>{t('inbox.discard')}</button>
        </div>
      )}

      {canSort && action === 'open' && (
        <form className="action-form" onSubmit={open}>
          <label htmlFor={`${id}-customer`}>{t('inbox.customer')}</label>
          <select id={`${id}-customer`} required value={customerId} disabled={!customers}
            onChange={(e) => setCustomerId(e.target.value)}>
            <option value="">{customers ? t('customer.choose') : t('customer.loading')}</option>
            {customers?.map((c) => (
              <option key={c.id} value={c.id}>{c.is_help_desk ? t('customer.helpDesk', { name: c.name }) : c.name}</option>
            ))}
          </select>
          <button type="submit" disabled={busy || !customerId}>{t('inbox.openSubmit')}</button>
          <button type="button" className="quiet" onClick={() => choose(null)}>{t('inbox.cancel')}</button>
        </form>
      )}
      {canSort && action === 'attach' && (
        <form className="action-form" onSubmit={attach}>
          <label htmlFor={`${id}-ticket`}>{t('inbox.ticketNumber')}</label>
          <input id={`${id}-ticket`} type="number" min={1} step={1} required inputMode="numeric"
            value={ticketNumber} onChange={(e) => setTicketNumber(e.target.value)} />
          <button type="submit" disabled={busy || !ticketNumber}>{t('inbox.attachSubmit')}</button>
          <button type="button" className="quiet" onClick={() => choose(null)}>{t('inbox.cancel')}</button>
        </form>
      )}
      {error !== null && (
        <p className="error" role="alert">{error === notATicket ? t('inbox.notATicket') : errorText(error)}</p>
      )}
    </li>
  )
}
